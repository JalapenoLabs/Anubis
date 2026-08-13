//! The Redis fanout: how one instance's publish reaches every instance.
//!
//! The in-process backend needs nothing here; it dispatches straight into the
//! registry. The Redis backend splits the job in two. Publishing goes out over
//! a reconnecting command connection, and a single background pump holds the
//! subscriber connection, follows the channels this process is listening to,
//! and dispatches everything it receives into the same registry. A publish
//! therefore reaches the publishing instance the same way it reaches every
//! other one, through Redis, so an event is never delivered twice locally and
//! never delivered in a different order to different listeners on one node.

use std::collections::HashSet;
use std::sync::Weak;
use std::time::Duration;

use futures_util::StreamExt;
use redis::aio::ConnectionManager;
use redis::{Client, RedisError};
use tokio::sync::mpsc::UnboundedReceiver;

use super::registry::Registry;

/// What every Anubis channel name is prefixed with inside Redis.
///
/// Redis pub/sub has one flat namespace that may be shared with anything else
/// the deployment runs, so the framework keeps to its own corner of it.
const REMOTE_PREFIX: &str = "anubis:realtime:";

/// How long the pump waits before its first reconnection attempt.
const RECONNECT_MIN: Duration = Duration::from_millis(250);

/// The longest the pump waits between reconnection attempts.
///
/// Five seconds keeps a recovered Redis picked up promptly while a Redis that
/// is genuinely down is retried a dozen times a minute rather than constantly.
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// A change in what this process wants delivered to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Interest {
    /// The first local subscriber of a channel arrived.
    Opened(String),
    /// The last local subscriber of a channel left.
    Closed(String),
}

/// The publishing half of the Redis backend.
///
/// [`ConnectionManager`] reconnects underneath, so a Redis restart surfaces as
/// a failed publish or two rather than as a permanently broken instance.
#[derive(Clone)]
pub(crate) struct RedisFanout {
    connection: ConnectionManager,
}

impl RedisFanout {
    /// Connects to Redis, and starts the pump that feeds `registry`.
    ///
    /// `interest` is the receiving half of the channel the registry announces
    /// its subscriptions on; the pump follows it to decide what Redis should
    /// deliver here.
    ///
    /// The connection is made here rather than lazily: a deployment that names
    /// a Redis it cannot reach has a configuration problem, and finding out at
    /// the first publish would mean losing events to discover it.
    ///
    /// # Errors
    /// Returns the Redis error when the URL does not parse or the server
    /// cannot be reached.
    pub(crate) async fn connect(
        url: &str,
        interest: UnboundedReceiver<Interest>,
        registry: Weak<Registry>,
    ) -> Result<Self, RedisError> {
        let client = Client::open(url)?;
        let connection = ConnectionManager::new(client.clone()).await?;

        tokio::spawn(pump(client, interest, registry));

        Ok(Self { connection })
    }

    /// Publishes one rendered event frame to every instance.
    ///
    /// # Errors
    /// Returns the Redis error when the command cannot be delivered.
    pub(crate) async fn publish(&self, channel: &str, frame: &str) -> Result<(), RedisError> {
        // Cloning is how `ConnectionManager` hands out a usable handle; the
        // connection itself is shared behind it.
        let mut connection = self.connection.clone();
        redis::cmd("PUBLISH")
            .arg(format!("{REMOTE_PREFIX}{channel}"))
            .arg(frame)
            .query_async::<()>(&mut connection)
            .await
    }
}

impl std::fmt::Debug for RedisFanout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection knows the URL, and the URL may carry a password.
        f.write_str("RedisFanout(..)")
    }
}

/// Holds the subscriber connection and dispatches what arrives on it.
///
/// Runs until the [`Registry`] it feeds is dropped, which happens when the
/// last [`super::Channels`] handle goes: an application that stops serving
/// stops pumping, without a shutdown call of its own.
async fn pump(client: Client, mut interest: UnboundedReceiver<Interest>, registry: Weak<Registry>) {
    let mut wanted = HashSet::new();
    let mut backoff = RECONNECT_MIN;

    loop {
        let connected = match client.get_async_pubsub().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(
                    error.message = %error,
                    "realtime fanout could not reach Redis: {{error.message}}",
                );
                if !wait_before_retry(&mut interest, &mut wanted, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };
        backoff = RECONNECT_MIN;

        let (mut commands, mut messages) = connected.split();

        // A reconnection has to re-declare everything, because the server that
        // held the old subscriptions is not the one answering now.
        let mut healthy = true;
        for channel in &wanted {
            if let Err(error) = commands
                .subscribe(format!("{REMOTE_PREFIX}{channel}"))
                .await
            {
                tracing::warn!(
                    error.message = %error,
                    realtime.channel = channel,
                    "realtime fanout could not resubscribe {{realtime.channel}}: {{error.message}}",
                );
                healthy = false;
                break;
            }
        }

        while healthy {
            tokio::select! {
                announced = interest.recv() => {
                    let Some(announced) = announced else { return };
                    healthy = apply(&mut commands, &mut wanted, announced).await;
                }
                message = messages.next() => {
                    let Some(message) = message else {
                        tracing::warn!("realtime fanout lost its Redis subscriber connection");
                        break;
                    };
                    let Some(registry) = registry.upgrade() else { return };
                    deliver(&registry, &message);
                }
            }
        }

        if !wait_before_retry(&mut interest, &mut wanted, backoff).await {
            return;
        }
    }
}

/// Applies one interest change to the live connection and the wanted set.
///
/// Returns `false` when the connection failed and has to be rebuilt. The
/// wanted set is updated either way, so the reconnection declares the truth.
async fn apply(
    commands: &mut redis::aio::PubSubSink,
    wanted: &mut HashSet<String>,
    interest: Interest,
) -> bool {
    let outcome = match &interest {
        Interest::Opened(channel) => {
            wanted.insert(channel.clone());
            commands
                .subscribe(format!("{REMOTE_PREFIX}{channel}"))
                .await
        }
        Interest::Closed(channel) => {
            wanted.remove(channel);
            commands
                .unsubscribe(format!("{REMOTE_PREFIX}{channel}"))
                .await
        }
    };

    match outcome {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                error.message = %error,
                "realtime fanout could not update its Redis subscriptions: {{error.message}}",
            );
            false
        }
    }
}

/// Hands one message from Redis to the local subscribers of its channel.
fn deliver(registry: &Registry, message: &redis::Msg) {
    let Some(channel) = message.get_channel_name().strip_prefix(REMOTE_PREFIX) else {
        // Nothing else subscribes on this connection, so this cannot happen
        // unless somebody publishes to our namespace by hand.
        return;
    };

    match message.get_payload::<String>() {
        Ok(frame) => registry.dispatch(channel, &frame),
        Err(error) => tracing::warn!(
            error.message = %error,
            realtime.channel = channel,
            "discarding a realtime message on {{realtime.channel}} that is not text: \
             {{error.message}}",
        ),
    }
}

/// Waits out a reconnection delay without falling behind on interest changes.
///
/// Returns `false` when the announcement channel closed, which means the
/// application dropped its [`super::Channels`] and the pump is done.
async fn wait_before_retry(
    interest: &mut UnboundedReceiver<Interest>,
    wanted: &mut HashSet<String>,
    delay: Duration,
) -> bool {
    let retry_at = tokio::time::sleep(delay);
    let mut retry_at = std::pin::pin!(retry_at);

    loop {
        tokio::select! {
            () = &mut retry_at => return true,
            announced = interest.recv() => match announced {
                None => return false,
                Some(Interest::Opened(channel)) => {
                    wanted.insert(channel);
                }
                Some(Interest::Closed(channel)) => {
                    wanted.remove(&channel);
                }
            },
        }
    }
}
