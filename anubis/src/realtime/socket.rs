//! The websocket endpoint browsers subscribe through.
//!
//! One socket carries every channel a browser is interested in. The session
//! cookie authenticates the upgrade, and each subscribe request is authorized
//! on its own against the name it asks for, so a socket can never outlive or
//! outgrow what the person behind it may see.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use super::Channels;
use super::channel::{Audience, ChannelName};
use super::protocol::{ClientFrame, ErrorCode, ServerFrame};
use super::registry::{Delivery, Subscription};
use crate::auth::{CurrentUser, User};
use crate::db::DbPool;
use crate::schema::team_memberships;

/// Where the endpoint is mounted, and what `crate::spa` reserves for it.
const PATH: &str = "/realtime";

/// How often the server pings an idle socket.
///
/// Frequent enough that a connection dropped by a proxy is noticed within a
/// minute, rare enough that a thousand idle sockets cost nothing.
const HEARTBEAT: Duration = Duration::from_secs(20);

/// How long a socket may go without a single frame before it is closed.
///
/// Three heartbeats: a browser that is merely busy answers the first or the
/// second ping, while a connection that is gone (a laptop closed mid-flight,
/// a network that vanished without a `FIN`) is reaped rather than holding its
/// subscriptions open forever.
const IDLE_TIMEOUT: Duration = Duration::from_mins(1);

/// How many channels one socket may hold.
///
/// A page watches a handful of channels. Sixty-four is far above any honest
/// client and stops one connection from taking a meaningful share of the
/// per-process bound on its own.
pub(crate) const MAX_CHANNELS_PER_SOCKET: usize = 64;

/// The largest frame a browser may send.
///
/// Every client frame is a subscribe or unsubscribe carrying one channel name,
/// which is a little over a hundred bytes. Four kilobytes is generous for that
/// and keeps the default sixty-four megabyte limit, which a client could make
/// the server buffer, out of reach. Only incoming messages are limited;
/// published payloads travel the other way.
const MAX_CLIENT_FRAME: usize = 4 * 1024;

/// How many outgoing frames may queue for one socket before publishers wait.
///
/// Backpressure stops here rather than propagating to publishers: when the
/// queue fills, the subscription task waits, falls behind the channel, and the
/// client is told it lagged. A publish never blocks on a slow browser.
const OUTGOING_BACKLOG: usize = 64;

/// Mounts `GET /realtime` on its own router.
///
/// Merge it into the application router alongside the framework's other
/// routers. The extensions the socket needs travel with it, so nothing else
/// has to be layered for it to work.
///
/// ```no_run
/// # fn compose(pool: anubis::db::DbPool, channels: anubis::realtime::Channels) -> axum::Router {
/// axum::Router::new().merge(anubis::realtime::router(pool, channels))
/// # }
/// ```
pub fn router(pool: DbPool, channels: Channels) -> Router {
    Router::new()
        .route(PATH, get(upgrade))
        .layer(Extension(channels))
        .layer(Extension(pool))
}

/// Everything one socket needs for the whole of its life.
#[derive(Clone)]
struct Session {
    user: User,
    channels: Channels,
    pool: DbPool,
}

/// Authenticates the upgrade, then hands the socket to [`serve`].
///
/// [`CurrentUser`] runs before the handshake is answered, so an unauthenticated
/// request is refused with the same `401` the REST endpoints answer and no
/// websocket is ever established.
async fn upgrade(
    CurrentUser(user): CurrentUser,
    Extension(channels): Extension<Channels>,
    Extension(pool): Extension<DbPool>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let session = Session {
        user,
        channels,
        pool,
    };
    upgrade
        .max_message_size(MAX_CLIENT_FRAME)
        .on_upgrade(move |socket| serve(socket, session))
}

/// Runs one socket until the client leaves, falls silent, or the server stops.
async fn serve(socket: WebSocket, session: Session) {
    let (mut sender, mut receiver) = socket.split();
    let (outgoing, mut outgoing_frames) = mpsc::channel::<Arc<str>>(OUTGOING_BACKLOG);
    let mut subscriptions: HashMap<String, JoinHandle<()>> = HashMap::new();

    let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    // A socket busy answering frames has already proved it is alive, so a tick
    // it spent elsewhere is skipped rather than fired back to back with the
    // next one.
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_frame = Instant::now();

    tracing::debug!(
        user.id = %session.user.id,
        "realtime socket opened for {{user.id}}",
    );

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break };
                last_frame = Instant::now();

                let Message::Text(text) = message else {
                    // Pongs answer our heartbeat, a close is handled by the
                    // stream ending, and the protocol has no binary frames.
                    continue;
                };

                let reply = answer(&text, &session, &mut subscriptions, &outgoing).await;
                if sender.send(Message::Text(reply.into())).await.is_err() {
                    break;
                }
            }
            frame = outgoing_frames.recv() => {
                let Some(frame) = frame else { break };
                if sender.send(Message::Text(frame.as_ref().into())).await.is_err() {
                    break;
                }
            }
            _beat = heartbeat.tick() => {
                if last_frame.elapsed() >= IDLE_TIMEOUT {
                    tracing::debug!(
                        user.id = %session.user.id,
                        realtime.idle_timeout_ms = IDLE_TIMEOUT.as_millis(),
                        "closing a realtime socket that answered nothing for \
                         {{realtime.idle_timeout_ms}}ms",
                    );
                    break;
                }
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    // Aborting drops each task's subscription, which is what releases the
    // channels this socket was holding open.
    for task in subscriptions.into_values() {
        task.abort();
    }
    let _closed = sender.send(Message::Close(None)).await;
}

/// Answers one client frame. Every frame gets exactly one reply.
async fn answer(
    text: &str,
    session: &Session,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
    outgoing: &mpsc::Sender<Arc<str>>,
) -> String {
    let Ok(frame) = serde_json::from_str::<ClientFrame>(text) else {
        return ServerFrame::Error {
            channel: None,
            code: ErrorCode::InvalidFrame,
            message: "That is not a frame this server understands.",
        }
        .render();
    };

    match frame {
        ClientFrame::Subscribe { channel } => {
            subscribe(channel, session, subscriptions, outgoing).await
        }
        ClientFrame::Unsubscribe { channel } => {
            if let Some(task) = subscriptions.remove(&channel) {
                task.abort();
            }
            // Unsubscribing from something this socket never held is not an
            // error: the client's intent is satisfied either way.
            ServerFrame::Unsubscribed { channel: &channel }.render()
        }
    }
}

/// Authorizes and opens one subscription.
async fn subscribe(
    channel: String,
    session: &Session,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
    outgoing: &mpsc::Sender<Arc<str>>,
) -> String {
    let Ok(name) = channel.parse::<ChannelName>() else {
        return ServerFrame::Error {
            channel: Some(&channel),
            code: ErrorCode::InvalidFrame,
            message: "That is not a channel name.",
        }
        .render();
    };

    // Subscribing twice is idempotent, so a client that resubscribes after a
    // reconnect it only half noticed does not accumulate subscriptions.
    if subscriptions.contains_key(&channel) {
        return ServerFrame::Subscribed { channel: &channel }.render();
    }

    if subscriptions.len() >= MAX_CHANNELS_PER_SOCKET {
        return ServerFrame::Error {
            channel: Some(&channel),
            code: ErrorCode::TooManyChannels,
            message: "This connection is already holding as many channels as it may.",
        }
        .render();
    }

    match access(session, &name).await {
        Access::Granted => {}
        // A team the user does not belong to and a team that does not exist
        // answer identically, exactly as the ownership-chain guards do.
        Access::Refused => {
            return ServerFrame::Error {
                channel: Some(&channel),
                code: ErrorCode::NotFound,
                message: "No such channel.",
            }
            .render();
        }
        Access::Unavailable => {
            return ServerFrame::Error {
                channel: Some(&channel),
                code: ErrorCode::Internal,
                message: "Something went wrong. Try subscribing again.",
            }
            .render();
        }
    }

    let Some(subscription) = session.channels.subscribe(&name) else {
        return ServerFrame::Error {
            channel: Some(&channel),
            code: ErrorCode::TooManyChannels,
            message: "This server is already holding as many channels as it may.",
        }
        .render();
    };

    let task = tokio::spawn(forward(subscription, channel.clone(), outgoing.clone()));
    let reply = ServerFrame::Subscribed { channel: &channel }.render();
    subscriptions.insert(channel, task);
    reply
}

/// Forwards one channel's deliveries to the socket until either end goes away.
async fn forward(
    mut subscription: Subscription,
    channel: String,
    outgoing: mpsc::Sender<Arc<str>>,
) {
    while let Some(delivery) = subscription.recv().await {
        let frame = match delivery {
            Delivery::Event(frame) => frame,
            Delivery::Lagged(missed) => {
                tracing::warn!(
                    realtime.channel = channel,
                    realtime.missed = missed,
                    "a realtime subscriber fell behind on {{realtime.channel}} and missed \
                     {{realtime.missed}} events",
                );
                let message = format!(
                    "This connection fell behind and missed {missed} events. \
                     Refetch to catch up."
                );
                Arc::from(
                    ServerFrame::Error {
                        channel: Some(&channel),
                        code: ErrorCode::Lagged,
                        message: &message,
                    }
                    .render(),
                )
            }
        };

        if outgoing.send(frame).await.is_err() {
            return;
        }
    }
}

/// Whether one user may listen to one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// The user may listen.
    Granted,
    /// The channel does not exist, or is not theirs. One answer covers both.
    Refused,
    /// The question could not be answered; the client may ask again.
    Unavailable,
}

/// Decides whether the session may listen to `channel`.
///
/// A user channel is self-only, so it needs no query at all. A team channel
/// needs the membership row, and the same query answers "no such team" and
/// "not your team", which is why one [`Access::Refused`] covers both.
async fn access(session: &Session, channel: &ChannelName) -> Access {
    let team_id = match channel.audience() {
        Audience::User(user_id) => {
            if user_id == session.user.id {
                return Access::Granted;
            }
            return Access::Refused;
        }
        Audience::Team(team_id) => team_id,
    };

    let mut connection = match session.pool.get().await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "realtime authorization failed: {{error.message}}",
            );
            return Access::Unavailable;
        }
    };

    let membership: Result<Option<Uuid>, _> = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.eq(session.user.id))
        .select(team_memberships::id)
        .first(&mut connection)
        .await
        .optional();

    match membership {
        Ok(Some(_membership)) => Access::Granted,
        Ok(None) => Access::Refused,
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "realtime authorization failed: {{error.message}}",
            );
            Access::Unavailable
        }
    }
}
