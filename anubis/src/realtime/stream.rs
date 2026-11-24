//! One Server-Sent Events stream: who asked for it, and what it sends.
//!
//! [`Subscriber`] is the extractor every stream starts from, the framework's
//! `/realtime` and an application's own endpoints alike: it requires a
//! signed-in session, reads where the client left off from `Last-Event-ID`,
//! and listens for the server starting to drain. [`EventStream`] is what a
//! handler answers with, built by [`super::Channels::stream`].

use std::collections::VecDeque;
use std::convert::Infallible;
use std::fmt::{self, Debug, Formatter};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::header::CACHE_CONTROL;
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::time::Instant;

use super::id::EventId;
use super::registry::{Delivery, Listener};
use super::replay::Start;
use crate::auth::{CurrentUser, User};
use crate::http::ApiError;
use crate::server::Drain;

/// The request header a reconnecting client names its last event in.
///
/// Spelled by the Server-Sent Events standard; a browser's `EventSource`
/// sends it on its own reconnects, and the framework's client sends it on
/// every connection after its first.
const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

/// Tells nginx, and the proxies that copied its convention, not to buffer.
///
/// A buffering proxy holds a stream's events until its buffer fills, which for
/// a quiet stream is never.
const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

/// No cache may keep a stream, and no proxy may rewrite it on the way.
///
/// `no-transform` is what keeps a proxy that compresses responses from
/// holding events back to fill a compression window.
const STREAM_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("no-cache, no-transform");

/// How long a stream may go without sending anything before it sends a
/// heartbeat comment.
///
/// Inside every idle timeout a stream is likely to cross: nginx closes a
/// proxied connection after sixty quiet seconds by default and Cloudflare
/// after a hundred, and a load balancer's is usually one of those two.
const HEARTBEAT: Duration = Duration::from_secs(20);

/// How long one stream lives before the server ends it and the client resumes.
///
/// A stream authorizes its channels when it opens, so its lifetime bounds how
/// long a session that was signed out elsewhere, or a member who was removed
/// from a team, keeps hearing events. Ending it costs nothing visible: the
/// client reconnects at once with its cursor and misses nothing. Five minutes
/// keeps that window short at one reconnect per tab per five minutes.
const LIFETIME: Duration = Duration::from_mins(5);

/// The most a stream's lifetime is lengthened by, chosen at random per stream.
///
/// Every tab reconnects together after a deploy; without the spread they
/// would keep reconnecting together every five minutes for good.
const LIFETIME_SPREAD_MILLIS: u64 = 60_000;

/// A signed-in session asking for a stream, and where it left off.
///
/// The extractor every stream handler takes: the session check is part of
/// the type, so a stream can never be mounted without one. It answers `401`
/// for a request with no session and the same `403` `CurrentUser` answers
/// for an account that must change its password first.
///
/// It also reads `Last-Event-ID`, which is how a reconnecting client asks to
/// resume: [`super::Channels::stream`] replays what the client missed, or
/// opens with a reset when the replay log no longer holds all of it. Under
/// [`crate::server::serve`] it also hears the server start to drain, so an
/// open stream ends at once rather than holding the shutdown for its whole
/// drain window.
///
/// ```no_run
/// use anubis::http::ApiError;
/// use anubis::realtime::{ChannelName, Channels, EventStream, Subscriber};
/// use axum::Extension;
/// use axum::extract::Path;
/// use uuid::Uuid;
///
/// /// `GET /api/v1/servers/{server_id}/logs`
/// async fn logs(
///     subscriber: Subscriber,
///     Path(server_id): Path<Uuid>,
///     Extension(channels): Extension<Channels>,
/// ) -> Result<EventStream, ApiError> {
///     // The application's own rule for who may read this server's logs,
///     // against `subscriber.user()`, goes here.
///     let stream = channels
///         .stream(subscriber.backfill(), [ChannelName::resource(server_id, "logs")])
///         .await?;
///     Ok(stream)
/// }
/// ```
pub struct Subscriber {
    user: User,
    cursor: Cursor,
    backfill: bool,
    drain: Option<Drain>,
}

/// What a request's `Last-Event-ID` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// No header: a first connection.
    Absent,
    /// An id this server renders.
    Read(EventId),
    /// Something else, which cannot be resumed from.
    Unreadable,
}

impl Subscriber {
    /// The signed-in user asking for the stream.
    ///
    /// For the handler's own authorization, before it opens the stream.
    #[must_use]
    pub fn user(&self) -> &User {
        &self.user
    }

    /// Replays everything each channel still keeps when this is a first connection.
    ///
    /// For a stream whose events are the data itself rather than news of a
    /// change to it: a log tail that should open with its recent lines, or a
    /// graph that should open full instead of blank. Without it a first
    /// connection opens with a reset, telling the client to read the current
    /// state itself. A reconnection resumes from its cursor either way.
    #[must_use]
    pub fn backfill(mut self) -> Self {
        self.backfill = true;
        self
    }

    /// Where a channel begins, given whether it continues the client's cursor.
    ///
    /// A channel that was not on the connection the cursor came from cannot
    /// resume from it: events on it before the cursor may never have reached
    /// the client, and events after it may predate what the client fetched.
    pub(crate) fn start(&self, continues: bool) -> Start {
        match self.cursor {
            Cursor::Read(cursor) if continues => Start::After(cursor),
            Cursor::Unreadable if continues => Start::Unknown,
            _ if self.backfill => Start::Backfill,
            _ => Start::Head,
        }
    }

    /// The server's drain signal, when the server sends one.
    pub(crate) fn into_drain(self) -> Option<Drain> {
        self.drain
    }
}

impl Debug for Subscriber {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscriber")
            .field("user", &self.user.id)
            .field("cursor", &self.cursor)
            .field("backfill", &self.backfill)
            .finish_non_exhaustive()
    }
}

impl<S> FromRequestParts<S> for Subscriber
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;

        let cursor = match parts.headers.get(LAST_EVENT_ID) {
            None => Cursor::Absent,
            Some(value) => match value.to_str() {
                // The standard has a client send nothing rather than an empty
                // id, so an empty one is a first connection all the same.
                Ok("") => Cursor::Absent,
                Ok(text) => EventId::parse(text).map_or(Cursor::Unreadable, Cursor::Read),
                Err(_not_text) => Cursor::Unreadable,
            },
        };
        if cursor == Cursor::Unreadable {
            tracing::debug!(
                user.id = %user.id,
                "a stream for {{user.id}} named a Last-Event-ID this server never sent; \
                 its channels open with a reset",
            );
        }

        Ok(Self {
            user,
            cursor,
            backfill: false,
            drain: parts.extensions.get::<Drain>().cloned(),
        })
    }
}

/// An open stream, ready to be answered with.
///
/// Built by [`super::Channels::stream`]; a handler returns it as is. It
/// answers `200` with `Content-Type: text/event-stream`, sends what the
/// stream opens with (refusals, resets, the replay, and a `live` frame
/// carrying the cursor), then each event as it is published, with a heartbeat
/// comment whenever it has been quiet for twenty seconds.
///
/// It ends itself, and the client resumes from its cursor, when its five
/// minute lifetime is up, when the server starts to drain, when it falls too
/// far behind, or when the delivery path under it broke.
pub struct EventStream {
    opening: VecDeque<Event>,
    feed: Feed,
}

/// What a stream delivers once its opening is sent.
struct Feed {
    listener: Listener,
    epoch: u64,
    /// The head of the sequence when the stream opened: anything at or before
    /// it was in the opening, or was not the stream's to send.
    head: u64,
    ends_at: Instant,
    drain: Option<Drain>,
}

impl EventStream {
    /// Builds a stream from what it opens with and what it delivers after.
    pub(crate) fn new(
        opening: VecDeque<Event>,
        listener: Listener,
        epoch: u64,
        head: u64,
        drain: Option<Drain>,
    ) -> Self {
        let spread = getrandom::u64().expect("the OS random source must be available")
            % LIFETIME_SPREAD_MILLIS;
        Self {
            opening,
            feed: Feed {
                listener,
                epoch,
                head,
                ends_at: Instant::now() + LIFETIME + Duration::from_millis(spread),
                drain,
            },
        }
    }

    /// The next event to send, or `None` when the stream ends.
    async fn next(&mut self) -> Option<Event> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }

        let feed = &mut self.feed;
        loop {
            tokio::select! {
                delivery = feed.listener.recv() => {
                    // A queue that ended was cut, and the client resumes.
                    let delivery = delivery?;
                    if delivery.id.epoch() != feed.epoch {
                        tracing::info!(
                            "ending a realtime stream: the sequence began a new epoch, \
                             so its client resumes with a reset",
                        );
                        return None;
                    }
                    // Already sent in the opening, or published before it.
                    if delivery.id.sequence() <= feed.head {
                        continue;
                    }
                    return Some(event_for(&delivery));
                }
                () = tokio::time::sleep_until(feed.ends_at) => return None,
                () = draining(feed.drain.as_mut()) => return None,
            }
        }
    }
}

/// Resolves once the server starts draining, or never without a signal.
async fn draining(drain: Option<&mut Drain>) {
    match drain {
        Some(drain) => drain.started().await,
        None => std::future::pending().await,
    }
}

/// The Server-Sent Event carrying one published event.
pub(crate) fn event_for(delivery: &Delivery) -> Event {
    Event::default()
        .id(delivery.id.to_string())
        .data(&*delivery.data)
}

impl Debug for EventStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream")
            .field("opening", &self.opening.len())
            .finish_non_exhaustive()
    }
}

impl IntoResponse for EventStream {
    fn into_response(self) -> Response {
        let events = futures_util::stream::unfold(self, |mut stream| async move {
            let event = stream.next().await?;
            Some((Ok::<_, Infallible>(event), stream))
        });
        let body =
            Sse::new(events).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("heartbeat"));

        (
            [
                (CACHE_CONTROL, STREAM_CACHE_CONTROL),
                (X_ACCEL_BUFFERING, HeaderValue::from_static("no")),
            ],
            body,
        )
            .into_response()
    }
}
