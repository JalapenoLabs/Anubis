//! The Redis backend's own tests: its wire formats, and its pump against a
//! Redis reached through a proxy that controls what passes.
//!
//! The pump tests need `REDIS_URL`, and skip with a note without it.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use redis::Client;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::{
    ANSWER_TIMEOUT, Interest, KEEPALIVE, Remote, net_changes, parse_entry, parse_message, pump,
    subscribe,
};
use crate::realtime::id::EventId;
use crate::realtime::registry::{Listener, Registry};
use crate::realtime::replay::{Cursor, MemoryLog, Start};
use crate::realtime::{ChannelName, Channels};

/// A byte budget nothing in these tests comes near.
const ROOMY: usize = 1024 * 1024;

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

#[test]
fn the_last_change_to_a_channel_in_a_batch_decides_it() {
    let opened = |channel: &str, generation| Interest::Opened {
        channel: channel.to_owned(),
        generation,
    };
    let closed = |channel: &str, generation| Interest::Closed {
        channel: channel.to_owned(),
        generation,
    };
    let mut wanted = HashMap::from([("kept".to_owned(), 1), ("dropped".to_owned(), 2)]);

    let (mut subscribe_to, mut let_go) = net_changes(
        &mut wanted,
        vec![
            // Closed and opened again: subscribed, under the new opening.
            opened("reopened", 3),
            closed("reopened", 3),
            opened("reopened", 4),
            // Opened and closed again: let go.
            opened("fleeting", 5),
            closed("fleeting", 5),
            closed("dropped", 2),
        ],
    );
    subscribe_to.sort();
    let_go.sort();

    assert_eq!(subscribe_to, vec![("reopened".to_owned(), 4)]);
    assert_eq!(let_go, vec!["dropped".to_owned(), "fleeting".to_owned()]);
    assert_eq!(
        wanted,
        HashMap::from([("kept".to_owned(), 1), ("reopened".to_owned(), 4)]),
        "a reconnection declares exactly what is still open",
    );
}

/// A channel `subscribe` reports confirmed is one Redis already delivers.
///
/// Redis answers a command naming several channels with one confirmation per
/// channel. Over a network that splits those confirmations across reads, one
/// left over from an earlier command can answer a later one before Redis has
/// run it. The proxy holds every request from the first reply on, so a
/// `subscribe` that returns before Redis holds its channels returns while some
/// of them are still waiting in the proxy.
#[tokio::test]
async fn subscribe_returns_only_once_redis_holds_every_channel() {
    let Some(redis_url) = redis_url("subscribe confirmations") else {
        return;
    };
    let proxy = Proxy::start(&redis_url).await;
    let client = Client::open(proxy.url.as_str()).expect("the proxy URL must parse");
    let (commands, _messages) = client
        .get_async_pubsub()
        .await
        .expect("Redis must be reachable through the proxy")
        .split();
    let registry = Registry::new(None);
    let nonce = Uuid::new_v4();
    // Hundreds of channels, the burst of every tab reconnecting after a deploy.
    let channels: Vec<(String, u64)> = (0..600)
        .map(|index| (format!("anubis:test:{nonce}:{index}"), 0))
        .collect();

    proxy.set(Passage::HoldAfterReply);
    let subscribing = async {
        let subscribed = subscribe(&commands, &Arc::downgrade(&registry), &channels).await;
        // Read the moment it returns, while anything held is still held.
        (subscribed, subscriber_counts(&redis_url, &channels).await)
    };
    let release = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        proxy.set(Passage::Open);
    };
    let ((subscribed, counts), ()) = tokio::join!(subscribing, release);

    assert!(subscribed, "the connection stayed healthy");
    let missing: Vec<&String> = channels
        .iter()
        .map(|(channel, _generation)| channel)
        .filter(|channel| counts.get(&pubsub_name(channel)).copied().unwrap_or(0) == 0)
        .collect();
    assert!(
        missing.is_empty(),
        "{} channels were reported confirmed before Redis held them, from {:?}",
        missing.len(),
        missing.first(),
    );
}

/// A subscriber connection that goes dark ends every stream, and is rebuilt.
///
/// A network that drops a path neither errors nor delivers, so the pump has to
/// notice on its own: through the keepalive while it is idle, and through the
/// bound on every answer while it is waiting on one, well before the keepalive
/// would. Time is paused only once the path is dark, so the test waits out
/// neither in earnest.
#[tokio::test]
async fn a_subscriber_connection_gone_dark_ends_every_stream_and_is_rebuilt() {
    let Some(redis_url) = redis_url("dark subscriber connection") else {
        return;
    };
    let proxy = Proxy::start(&redis_url).await;
    let (announcements, interest) = mpsc::unbounded_channel();
    let registry = Registry::new(Some(announcements));
    let client = Client::open(proxy.url.as_str()).expect("the proxy URL must parse");
    let _pump = tokio::spawn(pump(client, interest, Arc::downgrade(&registry)));
    let nonce = Uuid::new_v4();
    let account = Uuid::new_v4();
    let channel = |name: &str| vec![format!("anubis:test:{nonce}:{name}")];

    // Idle: only the keepalive can notice.
    let mut idle = live(&registry, account, &channel("idle")).await;
    proxy.set(Passage::Dark);
    tokio::time::pause();
    let noticed = KEEPALIVE + ANSWER_TIMEOUT + Duration::from_secs(1);
    let ended = tokio::time::timeout(noticed, idle.recv()).await;
    assert!(
        matches!(ended, Ok(None)),
        "an idle stream must end once the keepalive goes unanswered",
    );
    tokio::time::resume();

    // The path comes back, and so does delivery.
    proxy.set(Passage::Open);
    let mut waiting = live(&registry, account, &channel("waiting")).await;

    // Waiting on Redis to confirm a new channel: the bound on that answer
    // notices, long before the next keepalive is due.
    proxy.set(Passage::Dark);
    tokio::time::pause();
    let (_opening, _refused) = registry
        .listen(account, &channel("opening"))
        .expect("within the account's share");
    let noticed = ANSWER_TIMEOUT + Duration::from_secs(1);
    let ended = tokio::time::timeout(noticed, waiting.recv()).await;
    assert!(
        matches!(ended, Ok(None)),
        "every stream must end once a subscription goes unanswered",
    );
}

/// The Redis log opens every channel exactly as the in-process log does.
///
/// The resume rules are stated twice, in `MemoryLog::opening` and in
/// snapshot.lua, so the same appends go to both, and every way a channel can
/// begin is asked of both: before and after the capacity drops events, and
/// after the budget lets a whole channel go.
#[tokio::test]
async fn the_redis_log_opens_every_channel_as_the_in_process_log_does() {
    let Some(redis_url) = redis_url("snapshot rules") else {
        return;
    };
    let remote = isolated_remote(&redis_url, 12).await;
    let mut memory = MemoryLog::new(0xfeed);

    // `a` keeps two events, so its oldest go; `b` keeps all of its own.
    let mut redis_epoch = 0;
    for (channel, capacity) in [("a", 2), ("b", 8), ("a", 2), ("a", 2), ("a", 2), ("b", 8)] {
        memory.append(channel, Arc::from("x"), capacity, ROOMY);
        let id = remote
            .publish(channel, "x", capacity, ROOMY)
            .await
            .expect("publishing must succeed");
        redis_epoch = id.epoch();
    }
    assert_same_openings(&memory, &remote, redis_epoch).await;

    // Each buffer costs 256 bytes and each one-byte event 65, so `a` and `b`
    // hold 386 each, and `d` brings the total to 1,093: past 800, which lets
    // go of `a`, the least recently published, and nothing else.
    memory.append("d", Arc::from("x"), 8, 800);
    remote
        .publish("d", "x", 8, 800)
        .await
        .expect("publishing must succeed");
    assert_same_openings(&memory, &remote, redis_epoch).await;
}

/// A new run of Redis costs the same however much the last one kept, and what
/// it kept is let go of a bounded share per publish, oldest run first.
#[tokio::test]
async fn a_new_run_sets_the_old_buffers_aside_and_lets_go_of_them_a_share_at_a_time() {
    let Some(redis_url) = redis_url("retired epochs") else {
        return;
    };
    let isolated = redis_database(&redis_url, 11);
    flush(&isolated).await;
    let remote = connect_remote(&isolated).await;
    let publish = async |channel: &str| {
        let id = remote
            .publish(channel, "x", 8, ROOMY)
            .await
            .expect("publishing must succeed");
        EventId::render_epoch(id.epoch())
    };

    let mut first = String::new();
    for index in 0..300 {
        first = publish(&format!("first:{index}")).await;
    }

    // More buffers than one publish lets go of, so some outlive the publish
    // that starts the next run.
    forge_new_run(&isolated).await;
    let second = publish("second").await;
    assert_ne!(second, first, "a new run is a new epoch");
    assert_eq!(buffers_of(&isolated, &first).await, 300 - 256);

    // A second new run before the first is let go of queues behind it.
    forge_new_run(&isolated).await;
    let third = publish("third:a").await;
    assert_eq!(buffers_of(&isolated, &first).await, 0);
    assert_eq!(buffers_of(&isolated, &second).await, 1);

    let _third = publish("third:b").await;
    assert_eq!(buffers_of(&isolated, &second).await, 0);
    assert_eq!(buffers_of(&isolated, &third).await, 2);
    let leftovers: Vec<String> = query(
        &isolated,
        redis::cmd("KEYS").arg("{anubis:realtime}:retired*"),
    )
    .await;
    assert!(
        leftovers.is_empty(),
        "nothing of a retired run is left: {leftovers:?}"
    );
}

/// A Redis user that may not run `INFO` is found out at startup.
///
/// The scripts read the server's run with it, and `INFO` is in the
/// `@dangerous` category a restricted user commonly lacks.
#[tokio::test]
async fn a_redis_user_that_may_not_run_info_is_refused_at_startup() {
    let Some(redis_url) = redis_url("Redis ACL") else {
        return;
    };
    let user = format!("anubis-test-{}", Uuid::new_v4().simple());
    let mut restricted = url::Url::parse(&redis_database(&redis_url, 10)).expect("a URL");
    restricted
        .set_username(&user)
        .expect("a Redis URL takes a user");
    restricted
        .set_password(Some("secret"))
        .expect("a Redis URL takes a password");
    let acl = |rules: &[&str]| {
        let mut command = redis::cmd("ACL");
        command.arg("SETUSER").arg(&user).arg(rules);
        command
    };

    let () = query(
        &redis_url,
        &acl(&["on", ">secret", "~*", "&*", "+@all", "-@dangerous"]),
    )
    .await;
    let refused = Channels::redis(restricted.as_str())
        .await
        .expect_err("a user that may not run INFO cannot run the scripts");
    assert!(refused.is_configuration(), "got: {refused}");

    // The documented grant is enough.
    let () = query(&redis_url, &acl(&["+info"])).await;
    let channels = Channels::redis(restricted.as_str())
        .await
        .expect("+@all -@dangerous +info runs everything the scripts need");
    channels
        .publish(&ChannelName::user(Uuid::new_v4(), "inbox"), "created", ())
        .await
        .expect("publishing must succeed");

    let () = query(&redis_url, redis::cmd("ACL").arg("DELUSER").arg(&user)).await;
}

/// Asks both logs about every way each channel can begin, and compares.
async fn assert_same_openings(memory: &MemoryLog, remote: &Remote, redis_epoch: u64) {
    let memory_epoch = memory.snapshot([]).epoch;
    let head = memory.snapshot([]).head;
    let starts = |epoch: u64| {
        let mut starts = Vec::new();
        for backfill in [false, true] {
            let mut cursors = vec![Cursor::Fresh, Cursor::Unknown];
            cursors.extend(
                (0..=head + 1).map(|sequence| Cursor::After(EventId::new(epoch, sequence))),
            );
            cursors.push(Cursor::After(EventId::new(epoch ^ 1, head)));
            for cursor in cursors {
                starts.push(Start { cursor, backfill });
            }
        }
        starts
    };

    for channel in ["a", "b", "c", "d"] {
        let in_memory: Vec<(&str, Start)> = starts(memory_epoch)
            .into_iter()
            .map(|start| (channel, start))
            .collect();
        let in_redis: Vec<(&str, Start)> = starts(redis_epoch)
            .into_iter()
            .map(|start| (channel, start))
            .collect();
        let expected = memory.snapshot(in_memory.iter().copied());
        let actual = remote
            .snapshot(&in_redis)
            .await
            .expect("the snapshot must run");

        assert_eq!(actual.head, expected.head);
        for ((_channel, start), (expected, actual)) in in_memory
            .iter()
            .zip(expected.openings.iter().zip(&actual.openings))
        {
            assert_eq!(actual, expected, "{channel} opening at {start:?}");
        }
    }
}

/// A `Remote` on a numbered database of its own, emptied first.
async fn isolated_remote(redis_url: &str, index: u8) -> Remote {
    let isolated = redis_database(redis_url, index);
    flush(&isolated).await;
    connect_remote(&isolated).await
}

/// A `Remote` on `url`, with nothing listening.
async fn connect_remote(url: &str) -> Remote {
    let (_announcements, interest) = mpsc::unbounded_channel();
    Remote::connect(url, interest, Weak::new())
        .await
        .expect("the test Redis must be reachable")
}

/// Makes the log look written by another run of Redis, as a restart does.
async fn forge_new_run(url: &str) {
    let () = query(
        url,
        redis::cmd("HSET")
            .arg("{anubis:realtime}:log")
            .arg("run_id")
            .arg("0000000000000000000000000000000000000000"),
    )
    .await;
}

/// How many buffers Redis still holds under `epoch`.
async fn buffers_of(url: &str, epoch: &str) -> usize {
    let pattern = format!("{{anubis:realtime}}:buffer:{epoch}:*");
    let keys: Vec<String> = query(url, redis::cmd("KEYS").arg(pattern)).await;
    keys.len()
}

/// Runs one command against `url` and reads its answer.
async fn query<Reply: redis::FromRedisValue>(url: &str, command: &redis::Cmd) -> Reply {
    let client = Client::open(url).expect("the Redis URL must parse");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("the test Redis must be reachable");
    command
        .query_async(&mut connection)
        .await
        .expect("the test Redis must answer")
}

/// Empties one numbered database.
async fn flush(url: &str) {
    let () = query(url, &redis::cmd("FLUSHDB")).await;
}

/// The same Redis, numbered database `index`.
fn redis_database(url: &str, index: u8) -> String {
    let mut parsed = url::Url::parse(url).expect("REDIS_URL must be a URL");
    parsed.set_path(&format!("/{index}"));
    parsed.to_string()
}

/// What the proxy lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Passage {
    /// Requests pass, and replies pass a segment at a time.
    Open,
    /// As `Open` until the next reply arrives, and `Held` from then on.
    HoldAfterReply,
    /// Replies pass; requests wait in the proxy.
    Held,
    /// Nothing passes either way and nothing closes, as when a network drops
    /// a path without a word.
    Dark,
}

/// A proxy in front of the test Redis that controls what passes, and how.
///
/// On loopback Redis hands a whole reply over in one read, which hides any bug
/// that needs a reply split across reads. A real network splits a large one,
/// so the proxy passes replies a segment at a time, a moment apart.
struct Proxy {
    /// The test Redis's URL, pointed at the proxy instead.
    url: String,
    passage: Arc<watch::Sender<Passage>>,
}

/// How much of a reply the proxy passes at once: one Ethernet segment's payload.
const SEGMENT: usize = 1448;

/// How long the proxy waits between the segments of a reply.
const PACE: Duration = Duration::from_millis(1);

impl Proxy {
    async fn start(redis_url: &str) -> Self {
        let mut url = url::Url::parse(redis_url).expect("REDIS_URL must be a URL");
        let upstream = format!(
            "{}:{}",
            url.host_str().expect("REDIS_URL must name a host"),
            url.port().unwrap_or(6379),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port must be free");
        let port = listener
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        url.set_host(Some("127.0.0.1"))
            .expect("a Redis URL takes a host");
        url.set_port(Some(port)).expect("a Redis URL takes a port");

        let passage = Arc::new(watch::channel(Passage::Open).0);
        let gate = Arc::clone(&passage);
        tokio::spawn(async move {
            while let Ok((client, _peer)) = listener.accept().await {
                let server = TcpStream::connect(&upstream)
                    .await
                    .expect("the test Redis must be reachable");
                let (client_reads, client_writes) = client.into_split();
                let (server_reads, server_writes) = server.into_split();
                tokio::spawn(requests(client_reads, server_writes, gate.subscribe()));
                tokio::spawn(replies(server_reads, client_writes, Arc::clone(&gate)));
            }
        });

        Self {
            url: url.to_string(),
            passage,
        }
    }

    fn set(&self, passage: Passage) {
        self.passage.send_replace(passage);
    }
}

/// Passes what the client sends on to Redis, whenever requests may pass.
async fn requests(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    mut gate: watch::Receiver<Passage>,
) {
    let mut buffer = vec![0; 64 * 1024];
    while let Ok(read) = from.read(&mut buffer).await
        && read > 0
    {
        let passes = gate
            .wait_for(|passage| matches!(passage, Passage::Open | Passage::HoldAfterReply))
            .await
            .is_ok();
        if !passes || to.write_all(&buffer[..read]).await.is_err() {
            return;
        }
    }
}

/// Passes what Redis answers back to the client, a segment at a time.
async fn replies(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    gate: Arc<watch::Sender<Passage>>,
) {
    let mut buffer = vec![0; 64 * 1024];
    let mut lit = gate.subscribe();
    while let Ok(read) = from.read(&mut buffer).await
        && read > 0
    {
        // Before the client can see the reply, so nothing it sends in answer
        // to it gets through.
        gate.send_if_modified(|passage| {
            let holds = *passage == Passage::HoldAfterReply;
            if holds {
                *passage = Passage::Held;
            }
            holds
        });
        for segment in buffer[..read].chunks(SEGMENT) {
            let passes = lit
                .wait_for(|passage| *passage != Passage::Dark)
                .await
                .is_ok();
            if !passes || to.write_all(segment).await.is_err() {
                return;
            }
            tokio::time::sleep(PACE).await;
        }
    }
}

/// Opens a stream on `channels` and waits for Redis to confirm them.
async fn live(registry: &Arc<Registry>, account: Uuid, channels: &[String]) -> Listener {
    let (mut listener, refused) = registry
        .listen(account, channels)
        .expect("within the account's share");
    assert!(refused.is_empty(), "the process has room: {refused:?}");
    let ready = tokio::time::timeout(Duration::from_secs(10), listener.ready()).await;
    assert!(
        matches!(ready, Ok(true)),
        "Redis must confirm {channels:?} through the proxy",
    );
    listener
}

/// How many subscribers Redis holds for each channel, by pub/sub name.
async fn subscriber_counts(redis_url: &str, channels: &[(String, u64)]) -> HashMap<String, u64> {
    let mut numsub = redis::cmd("PUBSUB");
    numsub.arg("NUMSUB");
    for (channel, _generation) in channels {
        numsub.arg(pubsub_name(channel));
    }
    query(redis_url, &numsub).await
}

/// The name a channel is subscribed under.
fn pubsub_name(channel: &str) -> String {
    format!("{}{channel}", super::PUBSUB_PREFIX)
}

/// `REDIS_URL`, or a logged skip naming `test`.
fn redis_url(test: &str) -> Option<String> {
    let url = std::env::var("REDIS_URL").ok();
    if url.is_none() {
        eprintln!("skipping {test}: REDIS_URL is not set");
    }
    url
}
