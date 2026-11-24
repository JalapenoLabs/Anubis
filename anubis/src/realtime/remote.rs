//! The Redis backend: one sequence, one replay log, and fanout to every instance.
//!
//! The in-process backend needs nothing here. Under Redis, a publish is one
//! script ([`PUBLISH_SCRIPT`]) that mints the next id, keeps the event in its
//! channel's buffer, keeps the deployment within its byte budget, and
//! publishes it, all atomically, so the order Redis delivers events in is the
//! order of the sequence. A stream opening decides how each channel opens,
//! and reads what that opening sends, with a second script
//! ([`SNAPSHOT_SCRIPT`]) that sees the head, the watermark, and every channel
//! it asks about at one instant. Each script's work is bounded by what it
//! sends or keeps, never by how much the log holds.
//!
//! Delivery is one background pump per process. It holds the subscriber
//! connection, follows the channels this process carries, and dispatches
//! everything it receives into the registry, so a publish reaches the
//! publishing instance the same way it reaches every other one, through
//! Redis, and no stream ever hears an event twice or out of order. It names
//! one channel per `SUBSCRIBE`, so each confirmation answers its own command,
//! and it bounds every answer it waits on, so a connection that went dark is
//! rebuilt within a minute.
//!
//! Nothing here is durable. The buffers live as long as Redis keeps its data,
//! and the epoch beside the sequence is what tells a client when it has not:
//! whenever the Redis server is on a different run from the one the log was
//! written under (a restart from any snapshot or file, or nothing at all, or a
//! replica promoted in its place), the log starts a new epoch, and every cursor
//! from the old one resumes as a reset.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::try_join_all;
use redis::aio::{ConnectionManager, ConnectionManagerConfig, PubSubSink};
use redis::{Client, ErrorKind, RedisError, RedisResult, Script};
use tokio::sync::mpsc::UnboundedReceiver;

use super::id::EventId;
use super::registry::{Delivery, Registry};
use super::replay::{BUFFER_OVERHEAD, Cursor, ENTRY_OVERHEAD, Entry, Opening, Snapshot, Start};

/// The publish script, after what both scripts share; see their heads.
const PUBLISH_SCRIPT: &str = concat!(include_str!("lua/log.lua"), include_str!("lua/publish.lua"),);

/// The snapshot script, after what both scripts share; see their heads.
const SNAPSHOT_SCRIPT: &str = concat!(
    include_str!("lua/log.lua"),
    include_str!("lua/snapshot.lua"),
);

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

/// How long a publish or a snapshot may wait on Redis before it fails.
///
/// Both scripts are bounded per call, so a healthy Redis answers in a few
/// milliseconds; the slowest honest call is a stream opening that reads every
/// kept event of a few dozen backfill channels. Ten seconds is past that by
/// far, and matches how long a stream waits to open, so a call that runs out
/// of it means Redis is gone rather than busy.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the pump waits before its first reconnection attempt.
const RECONNECT_MIN: Duration = Duration::from_millis(250);

/// How often the pump checks that an idle subscriber connection still answers.
///
/// A connection a network black-holed never errors and never delivers, so
/// without a check it would look idle forever while every stream on the
/// instance heard nothing. Thirty seconds, plus [`ANSWER_TIMEOUT`] for the
/// `PING` itself, bounds that to under a minute.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// How long Redis may take to answer the subscriber connection before the
/// connection is taken for dead and rebuilt.
///
/// Bounds every wait the pump has on that connection: the keepalive `PING`,
/// and each `SUBSCRIBE` and `UNSUBSCRIBE`. Without it, an interest change sent
/// down a connection that went dark would park the pump, and delivery to every
/// stream on the instance, until the kernel gave up on the connection, which
/// on Linux's defaults is a quarter of an hour. A healthy Redis answers even
/// thousands of subscriptions in milliseconds.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The most interest changes the pump applies in one go.
///
/// Every change waiting is sent together, so a burst of streams opening, every
/// tab after a deploy, costs a round trip per batch rather than one per
/// channel. The bound keeps the pump coming back to deliver events between
/// batches.
const INTEREST_BATCH: usize = 512;

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
    /// The connection is made, and one empty snapshot read, here rather than
    /// lazily: a deployment that names a Redis it cannot reach, or one whose
    /// user may not run what the scripts run, has a configuration problem, and
    /// finding out at the first publish would mean losing events to discover
    /// it. The scripts ask Redis which run it is on with `INFO`, which a user
    /// restricted with `-@dangerous` may not run, so this is where that shows.
    ///
    /// # Errors
    /// Returns the Redis error when the URL does not parse, the server cannot
    /// be reached, or the scripts cannot run.
    pub(crate) async fn connect(
        url: &str,
        interest: UnboundedReceiver<Interest>,
        registry: Weak<Registry>,
    ) -> Result<Self, RedisError> {
        let client = Client::open(url)?;
        let config = ConnectionManagerConfig::new().set_response_timeout(Some(COMMAND_TIMEOUT));
        let connection = ConnectionManager::new_with_config(client.clone(), config).await?;
        let remote = Self {
            connection,
            publish: Script::new(PUBLISH_SCRIPT),
            snapshot: Script::new(SNAPSHOT_SCRIPT),
        };
        remote.snapshot(&[]).await?;

        tokio::spawn(pump(client, interest, registry));
        Ok(remote)
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
            .key(key("retired"))
            .arg(candidate_epoch())
            .arg(channel)
            .arg(data)
            .arg(capacity)
            .arg(budget)
            .arg(format!("{PUBSUB_PREFIX}{channel}"))
            .arg(KEY_PREFIX)
            .arg(ENTRY_OVERHEAD)
            .arg(BUFFER_OVERHEAD)
            .invoke_async(&mut connection)
            .await?;

        Ok(EventId::new(stored_epoch(&epoch)?, sequence))
    }

    /// Decides how each `(channel, start)` opens, and reads what it sends, at
    /// one instant.
    ///
    /// # Errors
    /// Returns the Redis error when the script cannot run, or answers with
    /// something Anubis did not write.
    pub(crate) async fn snapshot(
        &self,
        requests: &[(&str, Start)],
    ) -> Result<Snapshot, RedisError> {
        let mut invocation = self.snapshot.key(key("log"));
        invocation
            .key(key("recent"))
            .key(key("floors"))
            .key(key("sizes"))
            .key(key("retired"))
            .arg(candidate_epoch())
            .arg(KEY_PREFIX);
        for (channel, start) in requests {
            let (begins, epoch, sequence) = match start.cursor {
                Cursor::Fresh => ("fresh", String::new(), 0),
                Cursor::Unknown => ("unknown", String::new(), 0),
                // Rendered as the log stores its own, so the script compares
                // epochs as text, exactly.
                Cursor::After(cursor) => (
                    "after",
                    EventId::render_epoch(cursor.epoch()),
                    cursor.sequence(),
                ),
            };
            invocation
                .arg(*channel)
                .arg(begins)
                .arg(epoch)
                .arg(sequence)
                .arg(u8::from(start.backfill));
        }

        let mut connection = self.connection.clone();
        let ((epoch, head), openings): SnapshotReply =
            invocation.invoke_async(&mut connection).await?;

        let openings = openings
            .into_iter()
            .map(|(reset, entries)| {
                let entries = entries
                    .iter()
                    .map(|entry| parse_entry(entry))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Opening {
                    reset: reset == 1,
                    entries,
                })
            })
            .collect::<Result<Vec<_>, RedisError>>()?;

        Ok(Snapshot {
            epoch: stored_epoch(&epoch)?,
            head,
            openings,
        })
    }
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection knows the URL, and the URL may carry a password.
        f.write_str("Remote(..)")
    }
}

/// One of the framework's keys.
fn key(name: &str) -> String {
    format!("{KEY_PREFIX}{name}")
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

/// What the snapshot script answers: the epoch and head, then each channel's
/// opening as whether it resets, `1` or `0`, and the entries it replays.
type SnapshotReply = ((String, u64), Vec<(u8, Vec<String>)>);

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
        let everything: Vec<(String, u64)> = wanted
            .iter()
            .map(|(channel, generation)| (channel.clone(), *generation))
            .collect();
        let mut healthy = subscribe(&commands, &registry, &everything).await;

        let mut keepalive =
            tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        while healthy {
            tokio::select! {
                _tick = keepalive.tick() => {
                    healthy = answered("PING", commands.ping::<redis::Value>()).await;
                }
                announced = interest.recv() => {
                    let Some(first) = announced else { return };
                    let mut batch = vec![first];
                    while batch.len() < INTEREST_BATCH {
                        let Ok(next) = interest.try_recv() else { break };
                        batch.push(next);
                    }
                    let (opened, closed) = net_changes(&mut wanted, batch);
                    healthy = unsubscribe(&commands, &closed).await
                        && subscribe(&commands, &registry, &opened).await;
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

/// Waits for Redis to answer on the subscriber connection, for at most
/// [`ANSWER_TIMEOUT`].
///
/// Returns whether it answered in time and without an error. Either failure
/// means the connection has to be rebuilt, and is logged here, naming the
/// `command` that went unanswered.
async fn answered<Reply>(
    command: &'static str,
    answer: impl Future<Output = RedisResult<Reply>>,
) -> bool {
    match tokio::time::timeout(ANSWER_TIMEOUT, answer).await {
        Ok(Ok(_reply)) => true,
        Ok(Err(error)) => {
            tracing::warn!(
                error.message = %error,
                redis.command = command,
                "realtime fanout's Redis subscriber connection failed a {{redis.command}}: \
                 {{error.message}}",
            );
            false
        }
        Err(_elapsed) => {
            tracing::warn!(
                redis.command = command,
                "realtime fanout's Redis subscriber connection did not answer a \
                 {{redis.command}} in time; rebuilding it",
            );
            false
        }
    }
}

/// Subscribes to `channels` and, once Redis confirms them, marks each live.
///
/// One `SUBSCRIBE` per channel, sent together. Redis answers a command naming
/// several channels with one confirmation per channel, while the client pairs
/// each reply with one command in the order they were sent, so the spare
/// confirmations of a wide command would answer whichever command came next,
/// before Redis had run it. One channel per command keeps every confirmation
/// paired with its own command, and sending them together still costs a
/// single round trip.
///
/// Returns `false` when the connection failed or stopped answering, and has
/// to be rebuilt.
async fn subscribe(
    commands: &PubSubSink,
    registry: &Weak<Registry>,
    channels: &[(String, u64)],
) -> bool {
    let sent: Vec<_> = channels
        .iter()
        .map(|(channel, _generation)| {
            let mut sink = commands.clone();
            let name = format!("{PUBSUB_PREFIX}{channel}");
            async move { sink.subscribe(name).await }
        })
        .collect();
    if !answered("SUBSCRIBE", try_join_all(sent)).await {
        return false;
    }

    // Every channel's own confirmation is in, so every publish Redis executes
    // from here on is delivered on this connection.
    if let Some(registry) = registry.upgrade() {
        for (channel, generation) in channels {
            registry.mark_ready(channel, *generation);
        }
    }
    true
}

/// Unsubscribes from `channels`, one `UNSUBSCRIBE` each, sent together.
///
/// One channel per command for the reason [`subscribe`] gives. Returns `false`
/// when the connection failed or stopped answering.
async fn unsubscribe(commands: &PubSubSink, channels: &[String]) -> bool {
    let sent: Vec<_> = channels
        .iter()
        .map(|channel| {
            let mut sink = commands.clone();
            let name = format!("{PUBSUB_PREFIX}{channel}");
            async move { sink.unsubscribe(name).await }
        })
        .collect();
    answered("UNSUBSCRIBE", try_join_all(sent)).await
}

/// Folds a batch of interest changes into the wanted set, and into what Redis
/// has to be told.
///
/// Changes apply in order, so the last one for a channel decides it: a
/// channel closed and opened again in one batch stays subscribed under its
/// new generation, and one opened and closed again is let go. Returns the
/// channels to subscribe to, each with its generation, and the channels to
/// let go of. The wanted set is updated here, before anything is sent, so a
/// connection that fails while applying them reconnects declaring the truth.
fn net_changes(
    wanted: &mut HashMap<String, u64>,
    batch: Vec<Interest>,
) -> (Vec<(String, u64)>, Vec<String>) {
    // Each channel's last word: `Some(generation)` to subscribe, `None` to let go.
    let mut last: HashMap<String, Option<u64>> = HashMap::new();
    for interest in batch {
        match interest {
            Interest::Opened {
                channel,
                generation,
            } => {
                wanted.insert(channel.clone(), generation);
                last.insert(channel, Some(generation));
            }
            Interest::Closed { channel, .. } => {
                wanted.remove(&channel);
                last.insert(channel, None);
            }
        }
    }

    let mut opened = Vec::new();
    let mut closed = Vec::new();
    for (channel, change) in last {
        match change {
            Some(generation) => opened.push((channel, generation)),
            None => closed.push(channel),
        }
    }
    (opened, closed)
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
mod tests;
