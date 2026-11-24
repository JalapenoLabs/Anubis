//! Realtime channels: the server publishes, browsers listen over Server-Sent Events.
//!
//! [`Channels`] is the one service events are published through, from a
//! handler or from a background job. Browsers receive them over one
//! session-authenticated event stream at `GET /realtime`, mounted by
//! [`router`], carrying every channel the page listens to. An application
//! serves streams of its own, a game server's log tail or a metrics feed,
//! from its own endpoints with [`Channels::stream`]. The wire contract, the
//! namespace, and running this across several instances are documented for
//! application authors in `docs/realtime.md`.
//!
//! ```no_run
//! # async fn example(channels: &anubis::realtime::Channels, team_id: uuid::Uuid)
//! # -> Result<(), anubis::realtime::Error> {
//! use anubis::realtime::ChannelName;
//! use serde_json::json;
//!
//! channels
//!     .publish(
//!         &ChannelName::team(team_id, "projects"),
//!         "created",
//!         json!({ "id": 42, "name": "Apollo" }),
//!     )
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The namespace
//!
//! Channel names are structured, never free-form: `team:{team_id}:{topic}`,
//! `user:{user_id}:{topic}`, or `resource:{id}:{topic}`. The audience in the
//! name *is* the authorization rule, so the server can answer a request
//! without a table of hand-written rules: a team channel admits that team's
//! members, a user channel admits that one user, and a resource channel is
//! served only by the application's own endpoint, which authorizes it. A
//! channel a client may not hear is refused exactly as a nonexistent one is,
//! so a browser cannot probe for teams it does not belong to. See
//! [`ChannelName`].
//!
//! # Ids and replay
//!
//! Every publish takes the next [`EventId`] of one sequence shared by every
//! channel, and is kept in its channel's buffer: the last 64 events, or as
//! many as [`ChannelsBuilder::retain`] gives its topic, within a byte budget
//! for the whole deployment. A client that reconnects names the last id it
//! heard in `Last-Event-ID` and receives every later event on its channels.
//! When the buffers no longer hold all of them, the channel opens with a
//! `reset` instead, and the client reads its state again. A replay is exact
//! or it is a reset; it is never a quiet gap.
//!
//! # The two backends
//!
//! Everything is in process by default: one sequence, buffers in memory, and
//! a publish reaches the streams on this instance, which is the right shape
//! for the single-instance deployment the framework targets out of the box.
//! Setting `REDIS_URL` moves the sequence, the buffers, and the fanout into
//! Redis, so a publish reaches every instance's streams and a client may
//! resume on a different instance from the one it left. Nothing else changes:
//! the same [`Channels`] API, the same frames, the same authorization.
//!
//! Redis holds no durable state. The buffers are a convenience for clients
//! that reconnect; a Redis that loses them starts a new epoch, and every
//! client that resumes afterwards opens with a reset and refetches.
//!
//! # Bounded everywhere
//!
//! A stream carries at most 64 channels, a process holds at most 16,384, an
//! event renders to at most 64 KiB, and a stream that falls 256 events behind
//! is closed and resumes. Channel names arrive from browsers, so every one of
//! those is a bound on what a client can make the server hold.

mod channel;
mod endpoint;
mod id;
mod protocol;
mod registry;
mod remote;
mod replay;
mod stream;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::{HashSet, VecDeque};
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::response::sse::Event;
use serde::Serialize;

use crate::config::AppConfig;
use crate::http::ApiError;
use protocol::{ErrorCode, Frame};
use registry::{Delivery, Registry};
use remote::Remote;
use replay::{MemoryLog, Opening, Retention, Start};

#[doc(inline)]
pub use channel::{Audience, ChannelName, ParseError};
#[doc(inline)]
pub use endpoint::{PATH, router};
#[doc(inline)]
pub use id::EventId;
#[doc(inline)]
pub use stream::{EventStream, Subscriber};

/// The most channels one stream may carry.
///
/// A page listens to a handful. Sixty-four is far above any honest client,
/// and stops one stream from taking a meaningful share of the per-process
/// bound on its own.
pub const MAX_CHANNELS_PER_STREAM: usize = 64;

/// The largest an event may render to, payload and all.
///
/// Every event is kept for replay, so its size is memory the deployment holds
/// for a while. Sixty-four kilobytes is far past a changed record or a log
/// line; anything larger belongs behind a REST read the event points at.
pub const MAX_EVENT_BYTES: usize = 64 * 1024;

/// How long opening a stream may wait on Redis before answering `503`.
///
/// Opening waits for Redis to confirm the subscriptions, then reads the
/// replay. Both are a round trip or two to a healthy Redis, so ten seconds
/// means Redis is unreachable, and the client is better off retrying than
/// waiting.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Publishes realtime events and opens streams of them. Cheap to clone.
///
/// Build one at startup with [`Channels::from_config`], or with
/// [`Channels::builder`] to give a topic its own replay capacity, hand it to
/// [`router`], and clone it into anything that publishes or streams. Every
/// clone shares one sequence, one replay log, and one fanout backend.
#[derive(Debug, Clone)]
pub struct Channels {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    registry: Arc<Registry>,
    retention: Retention,
    backend: Backend,
}

/// Where the sequence, the replay log, and the fanout live.
#[derive(Debug)]
enum Backend {
    /// In this process. The lock covers the log and the dispatch that follows
    /// each append, which is what makes a stream's opening exact.
    InProcess(Mutex<MemoryLog>),
    /// In Redis, shared by every instance.
    Redis(Remote),
}

impl Channels {
    /// Channels served entirely inside this process, with the default retention.
    ///
    /// Publishing reaches the streams on this instance and nothing else, which
    /// is exactly right until an application runs more than one.
    #[must_use]
    pub fn in_process() -> Self {
        Self::builder().build_in_process()
    }

    /// Channels kept and fanned out through Redis, with the default retention.
    ///
    /// `url` is a `redis://` or `rediss://` URL, from `REDIS_URL`.
    ///
    /// # Errors
    /// See [`ChannelsBuilder::build_redis`].
    pub async fn redis(url: &str) -> Result<Self, Error> {
        Self::builder().build_redis(url).await
    }

    /// The channels the configuration asks for, with the default retention.
    ///
    /// # Errors
    /// See [`ChannelsBuilder::build`].
    pub async fn from_config(config: &AppConfig) -> Result<Self, Error> {
        Self::builder().build(config).await
    }

    /// Starts a service whose replay retention is set by hand.
    #[must_use]
    pub fn builder() -> ChannelsBuilder {
        ChannelsBuilder::default()
    }

    /// Publishes `event` with `payload` to everyone listening on `channel`.
    ///
    /// Returns the event's id. The event is kept in the channel's replay
    /// buffer whether or not anyone is listening, so a client that reconnects
    /// still hears it; a publisher never has to know who is connected. The
    /// event name is a short verb the client branches on, such as `created` or
    /// `updated`, and the payload is any serializable value, typically the
    /// changed record in the shape the REST API answers with.
    ///
    /// Publishing is not transactional: publish after the commit, or from a
    /// job the commit enqueued, or a client may hear about a write that rolled
    /// back.
    ///
    /// # Errors
    /// Returns an [`Error`] when `payload` cannot be serialized, when the
    /// event renders past [`MAX_EVENT_BYTES`], or when the Redis backend
    /// cannot take the publish.
    pub async fn publish(
        &self,
        channel: &ChannelName,
        event: &str,
        payload: impl Serialize + Send,
    ) -> Result<EventId, Error> {
        let payload = serde_json::to_value(payload)
            .map_err(|source| Error::new(ErrorKind::Payload(source)))?;
        let name = channel.to_string();
        let data = Frame::Event {
            channel: &name,
            event,
            payload: &payload,
        }
        .render();
        if data.len() > MAX_EVENT_BYTES {
            return Err(Error::new(ErrorKind::TooLarge(data.len())));
        }

        let capacity = self.inner.retention.capacity(channel);
        let budget = self.inner.retention.budget();
        match &self.inner.backend {
            Backend::InProcess(log) => Ok(self.append(log, &name, data, capacity, budget)),
            // Deliberately not dispatched locally as well: the publish comes
            // back to this instance through Redis like any other, so local
            // streams hear each event once and in the same order as remote
            // ones.
            Backend::Redis(remote) => remote
                .publish(&name, &data, capacity, budget)
                .await
                .map_err(|source| Error::new(ErrorKind::Transport(Some(source)))),
        }
    }

    /// Keeps one event in process and delivers it, under the log's lock.
    fn append(
        &self,
        log: &Mutex<MemoryLog>,
        channel: &str,
        data: String,
        capacity: usize,
        budget: usize,
    ) -> EventId {
        let data: Arc<str> = Arc::from(data);
        let mut log = lock(log);
        let id = log.append(channel, Arc::clone(&data), capacity, budget);
        // Under the same lock a stream opens under, so an opening stream finds
        // this event in its snapshot or on its queue, and never in neither.
        self.inner
            .registry
            .dispatch(channel, &Delivery { id, data });
        id
    }

    /// Opens a stream of `channels` for a request the handler has authorized.
    ///
    /// The helper behind an application's own stream endpoints, such as a
    /// game server's log tail at `GET /api/v1/servers/{server_id}/logs`. The
    /// [`Subscriber`] extractor has already required a session; authorizing
    /// the channels is the handler's, which is why any audience may be passed
    /// here, `resource` channels above all. See [`Subscriber`] for a handler.
    ///
    /// A first connection opens each channel with a reset, or with whatever
    /// the channel keeps when [`Subscriber::backfill`] asked for it. A
    /// reconnection replays what it missed, or opens with a reset when the
    /// replay log no longer holds all of it. Then events follow as they are
    /// published, with heartbeats in between, until the stream's lifetime is
    /// up or the server starts to drain, and the client resumes. Declare the
    /// route untimed with [`crate::server::Hardening::untimed`], as the
    /// starter does for `/realtime`: the stream's duration is the client's.
    ///
    /// # Errors
    /// Returns an [`Error`] when `channels` names more than
    /// [`MAX_CHANNELS_PER_STREAM`], or when the Redis backend cannot open the
    /// stream within ten seconds. Both convert into an [`ApiError`], `400`
    /// and `503`, so a handler can answer with `?`.
    pub async fn stream(
        &self,
        subscriber: Subscriber,
        channels: impl IntoIterator<Item = ChannelName>,
    ) -> Result<EventStream, Error> {
        let start = subscriber.start(true);
        let requests: Vec<(ChannelName, Start)> = channels
            .into_iter()
            .map(|channel| (channel, start))
            .collect();
        if requests.len() > MAX_CHANNELS_PER_STREAM {
            return Err(Error::new(ErrorKind::TooManyChannels(requests.len())));
        }
        self.open(subscriber, requests, Vec::new()).await
    }

    /// Opens a stream: registers its queue, reads its replay, and builds its opening.
    ///
    /// `refusals` are channels the caller already refused, sent first so the
    /// client hears why a channel it asked for is silent.
    pub(crate) async fn open(
        &self,
        subscriber: Subscriber,
        requests: Vec<(ChannelName, Start)>,
        refusals: Vec<Refusal>,
    ) -> Result<EventStream, Error> {
        // A channel named twice would be queued twice and delivered twice.
        let mut seen = HashSet::new();
        let requests: Vec<(String, Start)> = requests
            .into_iter()
            .map(|(channel, start)| (channel.to_string(), start))
            .filter(|(name, _start)| seen.insert(name.clone()))
            .collect();
        let names: Vec<String> = requests.iter().map(|(name, _start)| name.clone()).collect();

        let (listener, full, mut snapshot, granted) = match &self.inner.backend {
            Backend::InProcess(log) => {
                // Registering and reading under the publish lock: every event
                // is in the snapshot or on the queue, and never in neither.
                let log = lock(log);
                let (listener, full) = self.inner.registry.listen(&names);
                let granted = without(&requests, &full);
                let snapshot = log.snapshot(
                    granted
                        .iter()
                        .map(|(name, start)| (name.as_str(), start.since())),
                );
                drop(log);
                (listener, full, snapshot, granted)
            }
            Backend::Redis(remote) => {
                let (mut listener, full) = self.inner.registry.listen(&names);
                let granted = without(&requests, &full);
                let deadline = tokio::time::Instant::now() + OPEN_TIMEOUT;

                // Redis confirms the subscriptions before the replay is read,
                // so an event published in between lands on the queue, where
                // the stream skips it if the replay already carried it.
                let confirmed = tokio::time::timeout_at(deadline, listener.ready()).await;
                if !matches!(confirmed, Ok(true)) {
                    return Err(Error::new(ErrorKind::Transport(None)));
                }

                let reads: Vec<(&str, Option<u64>)> = granted
                    .iter()
                    .map(|(name, start)| (name.as_str(), start.since()))
                    .collect();
                let snapshot = tokio::time::timeout_at(deadline, remote.snapshot(&reads))
                    .await
                    .map_err(|_elapsed| Error::new(ErrorKind::Transport(None)))?
                    .map_err(|source| Error::new(ErrorKind::Transport(Some(source))))?;
                (listener, full, snapshot, granted)
            }
        };

        let mut opening = VecDeque::new();
        let full = full
            .into_iter()
            .map(|channel| Refusal::new(channel, ErrorCode::TooManyChannels));
        for refusal in refusals.into_iter().chain(full) {
            opening.push_back(refusal.event());
        }

        let buffers = std::mem::take(&mut snapshot.buffers);
        let mut replay = Vec::new();
        for ((channel, start), buffer) in granted.iter().zip(buffers) {
            match snapshot.opening(*start, buffer) {
                Opening::Reset => {
                    opening.push_back(Event::default().data(Frame::Reset { channel }.render()));
                }
                Opening::Replay(entries) => replay.extend(entries),
            }
        }

        // One channel's events are in order already; across channels, the
        // sequence is what orders them, exactly as live delivery does.
        replay.sort_by_key(|entry| entry.sequence);
        for entry in replay {
            opening.push_back(stream::event_for(&Delivery {
                id: EventId::new(snapshot.epoch, entry.sequence),
                data: entry.data,
            }));
        }

        let cursor = EventId::new(snapshot.epoch, snapshot.head);
        opening.push_back(
            Event::default()
                .id(cursor.to_string())
                .data(Frame::Live.render()),
        );

        Ok(EventStream::new(
            opening,
            listener,
            snapshot.epoch,
            snapshot.head,
            subscriber.into_drain(),
        ))
    }
}

/// `requests` less the channels in `refused`, in their order.
fn without(requests: &[(String, Start)], refused: &[String]) -> Vec<(String, Start)> {
    requests
        .iter()
        .filter(|(name, _start)| !refused.contains(name))
        .cloned()
        .collect()
}

/// Locks the in-process log, recovering what a panicking holder left behind.
///
/// A panic mid-append can at worst leave one buffer's byte count stale, which
/// lets the budget run a little early or late; refusing to publish for the rest
/// of the process would be the larger failure.
fn lock(log: &Mutex<MemoryLog>) -> MutexGuard<'_, MemoryLog> {
    log.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A channel a stream will not carry, and why.
#[derive(Debug, Clone)]
pub(crate) struct Refusal {
    channel: String,
    code: ErrorCode,
}

impl Refusal {
    pub(crate) fn new(channel: impl Into<String>, code: ErrorCode) -> Self {
        Self {
            channel: channel.into(),
            code,
        }
    }

    /// The error frame that tells the client.
    fn event(&self) -> Event {
        let message = match self.code {
            ErrorCode::InvalidChannel => "That is not a channel name.",
            // Identical for a channel that does not exist and one the
            // subscriber may not hear, so a refusal never confirms a team.
            ErrorCode::NotFound => "No such channel.",
            ErrorCode::TooManyChannels => {
                "This server is already holding as many channels as it may."
            }
        };
        Event::default().data(
            Frame::Error {
                channel: &self.channel,
                code: self.code,
                message,
            }
            .render(),
        )
    }
}

/// Builds a [`Channels`] service with replay retention set by hand.
///
/// Every channel keeps its last 64 events by default, and the deployment
/// keeps 64 MiB of events across every channel. A topic whose events are the
/// data itself, a log tail or a metrics feed, usually wants more of its own:
///
/// ```no_run
/// # async fn boot(config: &anubis::config::AppConfig) -> Result<(), anubis::realtime::Error> {
/// let channels = anubis::realtime::Channels::builder()
///     // The last thousand lines of every server's log.
///     .retain("logs", 1_000)
///     // Fifteen minutes of one sample a second.
///     .retain("resources", 900)
///     .replay_budget(512 * 1024 * 1024)
///     .build(config)
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// Under Redis the publisher's numbers are the ones applied, so every
/// instance of an application should build its service the same way, which
/// it does by running the same code.
#[derive(Debug, Default)]
pub struct ChannelsBuilder {
    retention: Retention,
}

impl ChannelsBuilder {
    /// Keeps the last `events` events of every channel whose topic is `topic`.
    ///
    /// The topic is the last part of a channel name, whatever its audience:
    /// `retain("logs", 1_000)` applies to `resource:{id}:logs` for every id.
    ///
    /// # Panics
    /// Panics when `topic` is not a valid topic or `events` is zero. Both are
    /// literals at the call site, so either is a programming error.
    #[must_use]
    pub fn retain(mut self, topic: impl AsRef<str>, events: usize) -> Self {
        let topic = topic.as_ref();
        assert!(
            channel::is_valid_topic(topic),
            "{topic:?} is not a valid channel topic",
        );
        assert!(events > 0, "a topic must keep at least one event");
        self.retention.retain(topic, events);
        self
    }

    /// Keeps at most `bytes` of events across every channel in the deployment.
    ///
    /// Past it, whole channels go, least recently published first, and a
    /// client resuming on one opens with a reset. Under Redis this is Redis
    /// memory shared by every instance; in process, it is this process's.
    ///
    /// # Panics
    /// Panics when `bytes` is zero.
    #[must_use]
    pub fn replay_budget(mut self, bytes: usize) -> Self {
        assert!(bytes > 0, "a replay budget must hold something");
        self.retention.set_budget(bytes);
        self
    }

    /// Builds channels served entirely inside this process.
    ///
    /// # Panics
    /// Panics when the operating system has no random source to mint the
    /// epoch from, which no platform the framework supports allows.
    #[must_use]
    pub fn build_in_process(self) -> Channels {
        let epoch = getrandom::u64().expect("the OS random source must be available");
        Channels {
            inner: Arc::new(Inner {
                registry: Registry::new(None),
                retention: self.retention,
                backend: Backend::InProcess(Mutex::new(MemoryLog::new(epoch))),
            }),
        }
    }

    /// Builds channels kept and fanned out through the Redis at `url`.
    ///
    /// Redis is contacted here rather than at the first publish, so a
    /// deployment that names an unreachable Redis fails at startup instead of
    /// silently losing events.
    ///
    /// # Errors
    /// Returns an [`Error`] when the URL does not parse or Redis cannot be
    /// reached.
    pub async fn build_redis(self, url: &str) -> Result<Channels, Error> {
        // The registry announces what this process carries, and the pump
        // follows those announcements. The pump holds only a weak handle on
        // the registry, so it stops when the last clone of the service goes.
        let (announcements, interest) = tokio::sync::mpsc::unbounded_channel();
        let registry = Registry::new(Some(announcements));
        let remote = Remote::connect(url, interest, Arc::downgrade(&registry))
            .await
            .map_err(|source| Error::new(ErrorKind::Configuration(source)))?;

        Ok(Channels {
            inner: Arc::new(Inner {
                registry,
                retention: self.retention,
                backend: Backend::Redis(remote),
            }),
        })
    }

    /// Builds the channels the configuration asks for.
    ///
    /// Redis wherever `REDIS_URL` is set, in process everywhere else, so
    /// choosing the backend is a deployment decision rather than a code change.
    ///
    /// # Errors
    /// Returns an [`Error`] when a configured Redis cannot be reached.
    pub async fn build(self, config: &AppConfig) -> Result<Channels, Error> {
        match &config.redis {
            Some(redis) => self.build_redis(redis.url()).await,
            None => Ok(self.build_in_process()),
        }
    }
}

/// A realtime event could not be published, a stream could not be opened, or
/// a backend could not be built.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    backtrace: Backtrace,
}

#[derive(Debug)]
enum ErrorKind {
    Configuration(redis::RedisError),
    /// Redis failed the operation, or did not answer in time.
    Transport(Option<redis::RedisError>),
    Payload(serde_json::Error),
    TooLarge(usize),
    TooManyChannels(usize),
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns `true` when the configured Redis could not be reached at all.
    ///
    /// This is a deployment problem: the URL is wrong, or Redis is not there.
    #[must_use]
    pub fn is_configuration(&self) -> bool {
        matches!(self.kind, ErrorKind::Configuration(_))
    }

    /// Returns `true` when Redis failed this one publish or stream, or did not
    /// answer in time.
    ///
    /// The connection reconnects underneath, so a later attempt may well
    /// succeed. Events are not queued for retry; see the module docs.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Transport(_))
    }

    /// Returns `true` when the payload could not be serialized to JSON.
    ///
    /// A programming error in the published type rather than a runtime fault.
    #[must_use]
    pub fn is_payload(&self) -> bool {
        matches!(self.kind, ErrorKind::Payload(_))
    }

    /// Returns `true` when the event rendered past [`MAX_EVENT_BYTES`].
    #[must_use]
    pub fn is_too_large(&self) -> bool {
        matches!(self.kind, ErrorKind::TooLarge(_))
    }

    /// Returns `true` when a stream was asked to carry more than
    /// [`MAX_CHANNELS_PER_STREAM`] channels.
    #[must_use]
    pub fn is_too_many_channels(&self) -> bool {
        matches!(self.kind, ErrorKind::TooManyChannels(_))
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            // Redis errors render the address but never the password.
            ErrorKind::Configuration(source) => {
                write!(f, "the realtime Redis backend is unusable: {source}")?;
            }
            ErrorKind::Transport(Some(source)) => {
                write!(f, "realtime could not reach Redis: {source}")?;
            }
            ErrorKind::Transport(None) => {
                write!(f, "realtime could not reach Redis in time")?;
            }
            ErrorKind::Payload(source) => {
                write!(f, "a realtime payload failed to serialize: {source}")?;
            }
            ErrorKind::TooLarge(bytes) => {
                write!(
                    f,
                    "a realtime event rendered to {bytes} bytes, past the \
                     {MAX_EVENT_BYTES}-byte limit",
                )?;
            }
            ErrorKind::TooManyChannels(count) => {
                write!(
                    f,
                    "a stream was asked to carry {count} channels, past the limit of \
                     {MAX_CHANNELS_PER_STREAM}",
                )?;
            }
        }
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Configuration(source) | ErrorKind::Transport(Some(source)) => Some(source),
            ErrorKind::Payload(source) => Some(source),
            ErrorKind::Transport(None) | ErrorKind::TooLarge(_) | ErrorKind::TooManyChannels(_) => {
                None
            }
        }
    }
}

impl From<Error> for ApiError {
    /// Renders a realtime failure in a handler as the response it deserves.
    ///
    /// Too many channels is the caller's to fix, `400`. Redis being away is
    /// the server's and passes, `503`, which a client retries. A payload that
    /// cannot be published is a programming error, `500`. The cause is logged
    /// on the way, because the body never names it.
    fn from(error: Error) -> Self {
        match error.kind {
            ErrorKind::TooManyChannels(_) => Self::validation(format!(
                "A stream may carry at most {MAX_CHANNELS_PER_STREAM} channels."
            )),
            ErrorKind::Configuration(_) | ErrorKind::Transport(_) => {
                tracing::error!(
                    error.message = %error,
                    "a realtime stream could not open: {{error.message}}",
                );
                Self::unavailable("Live updates are unavailable right now. Try again shortly.")
            }
            ErrorKind::Payload(_) | ErrorKind::TooLarge(_) => {
                tracing::error!(
                    error.message = %error,
                    "a realtime event could not be published: {{error.message}}",
                );
                Self::internal()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{ChannelName, Channels, MAX_EVENT_BYTES};

    #[tokio::test]
    async fn every_publish_takes_the_next_id_of_one_sequence() {
        let channels = Channels::in_process();
        let team = ChannelName::team(Uuid::new_v4(), "projects");
        let user = ChannelName::user(Uuid::new_v4(), "inbox");

        let first = channels.publish(&team, "created", json!({})).await;
        let second = channels.publish(&user, "created", json!({})).await;

        let first = first
            .expect("publishing in process never fails")
            .to_string();
        let second = second
            .expect("publishing in process never fails")
            .to_string();
        let (first_epoch, first_sequence) = first.split_once('-').expect("an id is epoch-sequence");
        let (second_epoch, second_sequence) =
            second.split_once('-').expect("an id is epoch-sequence");
        assert_eq!(first_epoch, second_epoch, "one service is one epoch");
        assert_eq!(first_sequence, "1");
        assert_eq!(second_sequence, "2");
    }

    #[tokio::test]
    async fn publishing_to_a_channel_nobody_hears_succeeds() {
        let channels = Channels::in_process();

        channels
            .publish(
                &ChannelName::user(Uuid::new_v4(), "inbox"),
                "created",
                json!({}),
            )
            .await
            .expect("a publisher never has to know who is connected");
    }

    #[tokio::test]
    async fn a_payload_that_cannot_serialize_is_named_as_such() {
        let channels = Channels::in_process();
        // A map keyed by something other than a string has no JSON rendering.
        let payload = std::collections::HashMap::from([(vec![1_u8], "value")]);

        let error = channels
            .publish(
                &ChannelName::user(Uuid::new_v4(), "inbox"),
                "created",
                payload,
            )
            .await
            .expect_err("the payload cannot be JSON");

        assert!(error.is_payload(), "got: {error}");
        assert!(!error.is_transport(), "got: {error}");
    }

    #[tokio::test]
    async fn an_event_past_the_size_limit_is_refused_before_it_is_kept() {
        let channels = Channels::in_process();
        let payload = Value::String("x".repeat(MAX_EVENT_BYTES));

        let error = channels
            .publish(
                &ChannelName::user(Uuid::new_v4(), "inbox"),
                "created",
                payload,
            )
            .await
            .expect_err("the event is too large to keep");

        assert!(error.is_too_large(), "got: {error}");
    }

    #[test]
    #[should_panic(expected = "not a valid channel topic")]
    fn retaining_an_invalid_topic_is_a_programming_error() {
        let _builder = Channels::builder().retain("Not A Topic", 10);
    }
}
