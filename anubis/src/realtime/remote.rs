//! The Redis backend: one sequence, one replay log, and fanout to every instance.
//!
//! The in-process backend needs nothing here. Under Redis, a publish is one
//! script ([`PUBLISH_SCRIPT`]) that mints the next id, keeps the event in its
//! channel's buffer, keeps the deployment within its byte budget, and
//! publishes it, all atomically, so the order Redis delivers events in is the
//! order of the sequence. A stream opening reads what it missed with a second
//! script ([`SNAPSHOT_SCRIPT`]) that sees the head, the watermark, and every
//! buffer it asks for at one instant.
//!
//! Delivery is one background pump per process. It holds the subscriber
//! connection, follows the channels this process carries, and dispatches
//! everything it receives into the registry, so a publish reaches the
//! publishing instance the same way it reaches every other one, through
//! Redis, and no stream ever hears an event twice or out of order.
//!
//! Nothing here is durable. The buffers live as long as Redis keeps its data,
//! and the epoch beside the sequence is what tells a client when it has not:
//! a Redis that comes back empty starts a new epoch, and every cursor from
//! the old one resumes as a reset.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use redis::aio::{ConnectionManager, PubSubSink};
use redis::{Client, ErrorKind, RedisError, Script};
use tokio::sync::mpsc::UnboundedReceiver;

use super::id::EventId;
use super::registry::{Delivery, Registry};
use super::replay::{BUFFER_OVERHEAD, BufferView, ENTRY_OVERHEAD, Entry, Snapshot};

/// The publish script; see the comment at its head.
const PUBLISH_SCRIPT: &str = include_str!("lua/publish.lua");

/// The snapshot script; see the comment at its head.
const SNAPSHOT_SCRIPT: &str = include_str!("lua/snapshot.lua");

/// What every Anubis pub/sub channel is prefixed with.
///
/// Redis pub/sub has one flat namespace that may be shared with anything else
/// the deployment runs, so the framework keeps to its own corner of it.
const PUBSUB_PREFIX: &str = "anubis:realtime:";

/// What every Anubis key is prefixed with.
///
/// The braces are a Redis Cluster hash tag: every key a script touches hashes
/// to one slot, so a script that lets go of another channel's buffer reaches a
/// key that lives beside the ones it was handed.
const KEY_PREFIX: &str = "{anubis:realtime}:";

/// How long the pump waits before its first reconnection attempt.
const RECONNECT_MIN: Duration = Duration::from_millis(250);

/// The longest the pump waits between reconnection attempts.
///
/// Five seconds keeps a recovered Redis picked up promptly while a Redis that
/// is genuinely down is retried a dozen times a minute rather than constantly.
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// A change in which channels this process wants delivered to it.
///
/// `generation` names one opening of a channel, so a confirmation the pump
/// sends for an opening that has since closed never marks a later one live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Interest {
    /// The first stream on this process opened the channel.
    Opened { channel: String, generation: u64 },
    /// The last stream on this process left it.
    Closed { channel: String, generation: u64 },
}

/// The command half of the Redis backend: publishing and snapshots.
///
/// [`ConnectionManager`] reconnects underneath, so a Redis restart surfaces as
/// a failed publish or two rather than as a permanently broken instance.
pub(crate) struct Remote {
    connection: ConnectionManager,
    publish: Script,
    snapshot: Script,
}

impl Remote {
    /// Connects to Redis, and starts the pump that feeds `registry`.
    ///
    /// `interest` is the receiving half of the channel the registry announces
    /// what it carries on; the pump follows it to decide what Redis should
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

        Ok(Self {
            connection,
            publish: Script::new(PUBLISH_SCRIPT),
            snapshot: Script::new(SNAPSHOT_SCRIPT),
        })
    }

    /// Mints the next id for `data` on `channel`, keeps it, and fans it out.
    ///
    /// `capacity` is how many events the channel keeps, and `budget` how many
    /// bytes the whole deployment keeps.
    ///
    /// # Errors
    /// Returns the Redis error when the script cannot run, or answers with
    /// something Anubis did not write.
    pub(crate) async fn publish(
        &self,
        channel: &str,
        data: &str,
        capacity: usize,
        budget: usize,
    ) -> Result<EventId, RedisError> {
        // Cloning is how `ConnectionManager` hands out a usable handle; the
        // connection itself is shared behind it.
        let mut connection = self.connection.clone();
        let (epoch, sequence): (String, u64) = self
            .publish
            .key(key("log"))
            .key(key("recent"))
            .key(key("floors"))
            .key(key("sizes"))
            .key(buffer_key(channel))
            .arg(candidate_epoch())
            .arg(channel)
            .arg(data)
            .arg(capacity)
            .arg(budget)
            .arg(format!("{PUBSUB_PREFIX}{channel}"))
            .arg(key("buffer:"))
            .arg(ENTRY_OVERHEAD)
            .arg(BUFFER_OVERHEAD)
            .invoke_async(&mut connection)
            .await?;

        Ok(EventId::new(stored_epoch(&epoch)?, sequence))
    }

    /// Reads what the log holds for each `(channel, since)`, at one instant.
    ///
    /// # Errors
    /// Returns the Redis error when the script cannot run, or answers with
    /// something Anubis did not write.
    pub(crate) async fn snapshot(
        &self,
        requests: &[(&str, Option<u64>)],
    ) -> Result<Snapshot, RedisError> {
        let mut invocation = self.snapshot.key(key("log"));
        invocation.key(key("floors"));
        for (channel, _since) in requests {
            invocation.key(buffer_key(channel));
        }
        invocation.arg(candidate_epoch());
        for (channel, since) in requests {
            // -1 asks for no entries; every real sequence is positive.
            let since = since.map_or_else(|| "-1".to_owned(), |since| since.to_string());
            invocation.arg(*channel).arg(since);
        }

        let mut connection = self.connection.clone();
        let ((epoch, head, watermark), views): ((String, u64, u64), SnapshotViews) =
            invocation.invoke_async(&mut connection).await?;

        let buffers = views
            .into_iter()
            .map(|(exists, floor, entries)| {
                let entries = entries
                    .iter()
                    .map(|entry| parse_entry(entry))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(BufferView {
                    exists: exists == 1,
                    floor,
                    entries,
                })
            })
            .collect::<Result<Vec<_>, RedisError>>()?;

        Ok(Snapshot {
            epoch: stored_epoch(&epoch)?,
            head,
            watermark,
            buffers,
        })
    }
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection knows the URL, and the URL may carry a password.
        f.write_str("Remote(..)")
    }
}

/// What the snapshot script answers per channel: whether its buffer exists,
/// its floor, and the entries after the sequence asked for.
type SnapshotViews = Vec<(u8, u64, Vec<String>)>;

/// One of the framework's keys.
fn key(name: &str) -> String {
    format!("{KEY_PREFIX}{name}")
}

/// The key holding one channel's buffer.
fn buffer_key(channel: &str) -> String {
    format!("{KEY_PREFIX}buffer:{channel}")
}

/// An epoch for the log to adopt if it has none yet.
///
/// Sixty-four random bits, so two deployments sharing nothing never mint ids
/// that read alike, and a Redis that lost its data never mints one it handed
/// out before.
fn candidate_epoch() -> String {
    let epoch = getrandom::u64().expect("the OS random source must be available");
    EventId::render_epoch(epoch)
}

/// Reads the epoch Redis holds, which only these scripts ever write.
fn stored_epoch(text: &str) -> Result<u64, RedisError> {
    EventId::parse_epoch(text).ok_or_else(|| foreign("the realtime epoch in Redis"))
}

/// Reads one kept entry, `"<sequence> <frame>"`.
fn parse_entry(text: &str) -> Result<Entry, RedisError> {
    let (sequence, data) = text
        .split_once(' ')
        .ok_or_else(|| foreign("a realtime replay entry"))?;
    let sequence = sequence
        .parse()
        .map_err(|_error| foreign("a realtime replay entry"))?;
    Ok(Entry {
        sequence,
        data: Arc::from(data),
    })
}

/// Reads one published message, `"<epoch> <sequence> <frame>"`.
fn parse_message(text: &str) -> Option<Delivery> {
    let (epoch, entry) = text.split_once(' ')?;
    let entry = parse_entry(entry).ok()?;
    Some(Delivery {
        id: EventId::new(EventId::parse_epoch(epoch)?, entry.sequence),
        data: entry.data,
    })
}

/// Names something in Redis that Anubis did not write the way it reads it.
fn foreign(what: &'static str) -> RedisError {
    RedisError::from((
        ErrorKind::UnexpectedReturnType,
        what,
        "it is not in the shape Anubis writes".to_owned(),
    ))
}

/// Holds the subscriber connection and dispatches what arrives on it.
///
/// Runs until the [`Registry`] it feeds is dropped, which happens when the
/// last [`super::Channels`] handle goes: an application that stops serving
/// stops pumping, without a shutdown call of its own.
async fn pump(client: Client, mut interest: UnboundedReceiver<Interest>, registry: Weak<Registry>) {
    // The channels this process carries, by the generation that opened them.
    let mut wanted: HashMap<String, u64> = HashMap::new();
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

        // A reconnection has to declare everything again, because the server
        // that held the old subscriptions is not the one answering now.
        let mut healthy = true;
        for (channel, generation) in &wanted {
            if !subscribe(&mut commands, &registry, channel, *generation).await {
                healthy = false;
                break;
            }
        }

        while healthy {
            tokio::select! {
                announced = interest.recv() => {
                    let Some(announced) = announced else { return };
                    healthy = apply(&mut commands, &mut wanted, &registry, announced).await;
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

        // Whatever was published while the connection was going never reached
        // this process, so every stream ends and its client resumes from the
        // replay log instead of carrying on with a hole in it.
        let Some(live) = registry.upgrade() else {
            return;
        };
        live.interrupt();
        drop(live);

        if !wait_before_retry(&mut interest, &mut wanted, backoff).await {
            return;
        }
    }
}

/// Subscribes to one channel and, once Redis confirms it, marks it live.
///
/// Returns `false` when the connection failed and has to be rebuilt.
async fn subscribe(
    commands: &mut PubSubSink,
    registry: &Weak<Registry>,
    channel: &str,
    generation: u64,
) -> bool {
    // The reply arrives once Redis has registered the subscription, so every
    // publish it executes from here on is delivered on this connection.
    if let Err(error) = commands
        .subscribe(format!("{PUBSUB_PREFIX}{channel}"))
        .await
    {
        tracing::warn!(
            error.message = %error,
            realtime.channel = channel,
            "realtime fanout could not subscribe to {{realtime.channel}}: {{error.message}}",
        );
        return false;
    }

    if let Some(registry) = registry.upgrade() {
        registry.mark_ready(channel, generation);
    }
    true
}

/// Applies one interest change to the live connection and the wanted set.
///
/// Returns `false` when the connection failed and has to be rebuilt. The
/// wanted set is updated either way, so the reconnection declares the truth.
async fn apply(
    commands: &mut PubSubSink,
    wanted: &mut HashMap<String, u64>,
    registry: &Weak<Registry>,
    interest: Interest,
) -> bool {
    match interest {
        Interest::Opened {
            channel,
            generation,
        } => {
            wanted.insert(channel.clone(), generation);
            subscribe(commands, registry, &channel, generation).await
        }
        Interest::Closed { channel, .. } => {
            wanted.remove(&channel);
            if let Err(error) = commands
                .unsubscribe(format!("{PUBSUB_PREFIX}{channel}"))
                .await
            {
                tracing::warn!(
                    error.message = %error,
                    "realtime fanout could not update its Redis subscriptions: {{error.message}}",
                );
                return false;
            }
            true
        }
    }
}

/// Hands one message from Redis to the local streams carrying its channel.
fn deliver(registry: &Registry, message: &redis::Msg) {
    let Some(channel) = message.get_channel_name().strip_prefix(PUBSUB_PREFIX) else {
        // Nothing else subscribes on this connection, so this cannot happen
        // unless somebody publishes to our namespace by hand.
        tracing::debug!("discarding a realtime message outside the framework's namespace");
        return;
    };

    let payload = match message.get_payload::<String>() {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(
                error.message = %error,
                realtime.channel = channel,
                "discarding a realtime message on {{realtime.channel}} that is not text: \
                 {{error.message}}",
            );
            return;
        }
    };

    let Some(delivery) = parse_message(&payload) else {
        tracing::warn!(
            realtime.channel = channel,
            "discarding a realtime message on {{realtime.channel}} that Anubis did not publish",
        );
        return;
    };
    registry.dispatch(channel, &delivery);
}

/// Waits out a reconnection delay without falling behind on interest changes.
///
/// Returns `false` when the announcement channel closed, which means the
/// application dropped its [`super::Channels`] and the pump is done.
async fn wait_before_retry(
    interest: &mut UnboundedReceiver<Interest>,
    wanted: &mut HashMap<String, u64>,
    delay: Duration,
) -> bool {
    let retry_at = tokio::time::sleep(delay);
    let mut retry_at = std::pin::pin!(retry_at);

    loop {
        tokio::select! {
            () = &mut retry_at => return true,
            announced = interest.recv() => match announced {
                None => return false,
                Some(Interest::Opened { channel, generation }) => {
                    wanted.insert(channel, generation);
                }
                Some(Interest::Closed { channel, .. }) => {
                    wanted.remove(&channel);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_entry, parse_message};
    use crate::realtime::id::EventId;

    #[test]
    fn a_published_message_reads_back_as_its_id_and_frame() {
        let delivery = parse_message(r#"00000000000000ab 42 {"type":"event","payload":"a b"}"#)
            .expect("a message Anubis published");

        assert_eq!(delivery.id, EventId::new(0xab, 42));
        assert_eq!(&*delivery.data, r#"{"type":"event","payload":"a b"}"#);
    }

    #[test]
    fn a_message_anubis_did_not_publish_is_discarded() {
        for text in [
            "",
            "no spaces",
            "00000000000000ab 42",
            "nothex 42 {}",
            "00000000000000ab x {}",
        ] {
            assert!(parse_message(text).is_none(), "{text:?} must be discarded");
        }
    }

    #[test]
    fn a_kept_entry_reads_back_as_its_sequence_and_frame() {
        let entry = parse_entry(r#"7 {"type":"event"}"#).expect("an entry Anubis kept");
        assert_eq!(entry.sequence, 7);
        assert_eq!(&*entry.data, r#"{"type":"event"}"#);

        let _refused = parse_entry("seven {}").expect_err("a sequence is a number");
        let _refused = parse_entry("7").expect_err("an entry carries a frame");
    }
}
