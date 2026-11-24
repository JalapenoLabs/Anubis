//! Replay: the recent events each channel keeps for clients that reconnect.
//!
//! Every published event is kept in its channel's buffer, so a client that
//! drops and reconnects with the id of the last event it heard receives what
//! it missed instead of refetching. Buffers are bounded twice: each channel
//! keeps its last [`DEFAULT_CAPACITY`] events (or its topic's own capacity),
//! and the deployment keeps [`DEFAULT_BUDGET`] bytes across every channel,
//! letting go of whole channels, least recently published first, past that.
//!
//! What was let go is remembered, which is what makes a resume honest. Each
//! buffer has a floor, the newest event it dropped to stay within capacity,
//! and the log has a watermark, the newest event of any channel it dropped
//! whole. A resume from before either is answered with a reset, because the
//! events in between may be gone, and a resume after both replays exactly.
//!
//! The rules live here once. The in-process backend keeps its buffers in a
//! [`MemoryLog`]; the Redis backend keeps them in Redis, under a script that
//! applies the same arithmetic, and both answer a [`Snapshot`] that
//! [`Snapshot::opening`] reads the same way.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use super::channel::ChannelName;
use super::id::EventId;

/// How many events a channel keeps unless its topic was given its own number.
///
/// A reconnect normally spans seconds: a network blip, a laptop lid, a deploy.
/// Sixty-four events covers that for any channel a person watches, and a
/// channel busier than that is one whose clients refetch on a reset anyway.
pub(crate) const DEFAULT_CAPACITY: usize = 64;

/// How many bytes of kept events the whole deployment holds by default.
///
/// Sixty-four megabytes holds a full buffer of one-kilobyte events for a
/// thousand channels, and far more partly full ones. In process it is memory
/// the server holds; under Redis it is shared by every instance.
pub(crate) const DEFAULT_BUDGET: usize = 64 * 1024 * 1024;

/// What one kept event costs the budget beyond its own text.
///
/// Its id, its place in a list, and the allocator's rounding. A budget that
/// counted text alone would let a flood of tiny events hold far more memory
/// than it claims to.
pub(crate) const ENTRY_OVERHEAD: usize = 64;

/// What one channel's buffer costs the budget before it holds anything.
///
/// The channel's name in the buffer map and the recency index, and under Redis
/// the key and the hash fields beside it, so a deployment where every user is
/// told one thing once still counts what those buffers cost.
pub(crate) const BUFFER_OVERHEAD: usize = 256;

/// How much each channel keeps, and how much the deployment keeps in all.
#[derive(Debug, Clone)]
pub(crate) struct Retention {
    capacity_by_topic: HashMap<String, usize>,
    budget: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            capacity_by_topic: HashMap::new(),
            budget: DEFAULT_BUDGET,
        }
    }
}

impl Retention {
    /// Gives every channel under `topic` its own capacity.
    pub(crate) fn retain(&mut self, topic: &str, capacity: usize) {
        self.capacity_by_topic.insert(topic.to_owned(), capacity);
    }

    /// Sets the deployment's byte budget.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    /// How many events `channel` keeps.
    pub(crate) fn capacity(&self, channel: &ChannelName) -> usize {
        self.capacity_by_topic
            .get(channel.topic())
            .copied()
            .unwrap_or(DEFAULT_CAPACITY)
    }

    /// How many bytes the deployment keeps across every channel.
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }
}

/// Where one channel of a new stream begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Start {
    /// At the head of the sequence: nothing is replayed, and the channel opens
    /// with a reset so the client reads its current state once the stream is
    /// live, which is the only way not to miss what happened while it was
    /// fetching.
    Head,
    /// At the oldest event the channel still keeps, for a stream whose events
    /// are the data itself, such as a log tail.
    Backfill,
    /// Just after this id: every later event the channel keeps, or a reset
    /// when the log can no longer vouch for all of them.
    After(EventId),
    /// After an id this server cannot read: a reset.
    Unknown,
}

impl Start {
    /// The sequence a snapshot replays after, or `None` to replay nothing.
    pub(crate) fn since(self) -> Option<u64> {
        match self {
            Self::Head | Self::Unknown => None,
            Self::Backfill => Some(0),
            Self::After(cursor) => Some(cursor.sequence()),
        }
    }
}

/// One kept event: its place in the sequence and its rendered frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) sequence: u64,
    pub(crate) data: Arc<str>,
}

/// What the log holds for some channels at one instant.
#[derive(Debug)]
pub(crate) struct Snapshot {
    /// The run of the sequence the log is on.
    pub(crate) epoch: u64,
    /// The newest sequence minted so far; a stream is live from here.
    pub(crate) head: u64,
    /// The newest event of any channel the budget let go of whole.
    pub(crate) watermark: u64,
    /// One view per channel asked for, in the order they were asked for.
    pub(crate) buffers: Vec<BufferView>,
}

/// What the log holds for one channel.
#[derive(Debug, Default)]
pub(crate) struct BufferView {
    /// Whether the channel has a buffer at all.
    pub(crate) exists: bool,
    /// The newest event the buffer dropped, or the watermark it was born under.
    pub(crate) floor: u64,
    /// The kept events after the sequence the request named, oldest first.
    pub(crate) entries: Vec<Entry>,
}

/// How one channel of a new stream opens.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// With a reset frame: the client reads the channel's state again.
    Reset,
    /// With these events, oldest first, which may be none at all.
    Replay(Vec<Entry>),
}

impl Snapshot {
    /// Decides how a channel that begins at `start` opens, given its buffer.
    ///
    /// The one statement of the resume rules. A cursor from another epoch, or
    /// from past the head, is not one this log minted; past that, a resume is
    /// exact unless the channel's floor or, for a channel with no buffer, the
    /// watermark shows that something after the cursor was let go.
    pub(crate) fn opening(&self, start: Start, buffer: BufferView) -> Opening {
        let cursor = match start {
            Start::Head | Start::Unknown => return Opening::Reset,
            Start::Backfill => return Opening::Replay(buffer.entries),
            Start::After(cursor) => cursor,
        };

        if cursor.epoch() != self.epoch || cursor.sequence() > self.head {
            return Opening::Reset;
        }

        let dropped_after_cursor = if buffer.exists {
            buffer.floor > cursor.sequence()
        } else {
            self.watermark > cursor.sequence()
        };
        if dropped_after_cursor {
            return Opening::Reset;
        }

        Opening::Replay(buffer.entries)
    }
}

/// The in-process backend's replay log: the sequence and every channel's buffer.
///
/// Held under the same lock as the dispatch that follows each append, so a
/// stream that registers and snapshots under it sees every event either in
/// its snapshot or on its queue, never in neither.
#[derive(Debug)]
pub(crate) struct MemoryLog {
    epoch: u64,
    sequence: u64,
    bytes: usize,
    watermark: u64,
    buffers: HashMap<Arc<str>, Buffer>,
    /// Channels by the sequence of their newest event, oldest first, which is
    /// the order the budget lets them go in.
    recency: BTreeMap<u64, Arc<str>>,
}

/// One channel's kept events.
#[derive(Debug)]
struct Buffer {
    entries: VecDeque<Entry>,
    floor: u64,
    bytes: usize,
}

impl MemoryLog {
    /// An empty log, on the run of the sequence named `epoch`.
    pub(crate) fn new(epoch: u64) -> Self {
        Self {
            epoch,
            sequence: 0,
            bytes: 0,
            watermark: 0,
            buffers: HashMap::new(),
            recency: BTreeMap::new(),
        }
    }

    /// Mints the next id and keeps `data` in `channel`'s buffer under it.
    ///
    /// Trims the buffer to `capacity`, then lets go of whole channels, least
    /// recently published first, until the log fits in `budget`.
    pub(crate) fn append(
        &mut self,
        channel: &str,
        data: Arc<str>,
        capacity: usize,
        budget: usize,
    ) -> EventId {
        self.sequence += 1;
        let sequence = self.sequence;
        let cost = data.len() + ENTRY_OVERHEAD;

        let name = if let Some((name, buffer)) = self.buffers.get_key_value(channel) {
            // The channel's place in the recency order moves to this event.
            if let Some(newest) = buffer.entries.back() {
                self.recency.remove(&newest.sequence);
            }
            Arc::clone(name)
        } else {
            let name: Arc<str> = Arc::from(channel);
            // A buffer born now cannot vouch for anything let go before it, so
            // it starts at the watermark rather than at zero.
            self.buffers.insert(
                Arc::clone(&name),
                Buffer {
                    entries: VecDeque::new(),
                    floor: self.watermark,
                    bytes: BUFFER_OVERHEAD,
                },
            );
            self.bytes += BUFFER_OVERHEAD;
            name
        };

        let buffer = self
            .buffers
            .get_mut(&name)
            .expect("the buffer was found or made just above");
        buffer.entries.push_back(Entry { sequence, data });
        buffer.bytes += cost;
        self.bytes += cost;

        while buffer.entries.len() > capacity {
            let Some(dropped) = buffer.entries.pop_front() else {
                break;
            };
            let freed = dropped.data.len() + ENTRY_OVERHEAD;
            buffer.floor = dropped.sequence;
            buffer.bytes -= freed;
            self.bytes -= freed;
        }

        self.recency.insert(sequence, name);
        self.fit(budget);

        EventId::new(self.epoch, sequence)
    }

    /// Lets go of whole channels, least recently published first, until the
    /// log fits in `budget`.
    fn fit(&mut self, budget: usize) {
        while self.bytes > budget {
            let Some((newest, name)) = self.recency.pop_first() else {
                break;
            };
            if let Some(buffer) = self.buffers.remove(&name) {
                self.bytes -= buffer.bytes;
            }
            self.watermark = self.watermark.max(newest);
        }
    }

    /// What the log holds for each `(channel, since)` asked for.
    ///
    /// A view carries the channel's kept events after `since`, or none when
    /// `since` is `None`.
    pub(crate) fn snapshot<'name>(
        &self,
        requests: impl IntoIterator<Item = (&'name str, Option<u64>)>,
    ) -> Snapshot {
        let buffers = requests
            .into_iter()
            .map(|(channel, since)| {
                let Some(buffer) = self.buffers.get(channel) else {
                    return BufferView::default();
                };
                let entries = since.map_or_else(Vec::new, |since| {
                    // Entries are in sequence order, so everything after
                    // `since` is one contiguous run at the back.
                    let first = buffer
                        .entries
                        .partition_point(|entry| entry.sequence <= since);
                    buffer.entries.range(first..).cloned().collect()
                });
                BufferView {
                    exists: true,
                    floor: buffer.floor,
                    entries,
                }
            })
            .collect();

        Snapshot {
            epoch: self.epoch,
            head: self.sequence,
            watermark: self.watermark,
            buffers,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BUFFER_OVERHEAD, ENTRY_OVERHEAD, MemoryLog, Opening, Snapshot, Start};
    use crate::realtime::id::EventId;

    const EPOCH: u64 = 0xfeed;

    fn data(text: &str) -> Arc<str> {
        Arc::from(text)
    }

    /// How `channel` opens for a stream beginning at `start`.
    fn open(log: &MemoryLog, channel: &str, start: Start) -> Opening {
        let mut snapshot: Snapshot = log.snapshot([(channel, start.since())]);
        let buffer = snapshot.buffers.remove(0);
        snapshot.opening(start, buffer)
    }

    /// The sequences a replay carries, or `None` for a reset.
    fn replayed(opening: Opening) -> Option<Vec<u64>> {
        match opening {
            Opening::Reset => None,
            Opening::Replay(entries) => Some(entries.iter().map(|entry| entry.sequence).collect()),
        }
    }

    #[test]
    fn one_sequence_orders_every_channel() {
        let mut log = MemoryLog::new(EPOCH);

        let first = log.append("team:a:projects", data("1"), 8, usize::MAX);
        let second = log.append("user:b:inbox", data("2"), 8, usize::MAX);
        let third = log.append("team:a:projects", data("3"), 8, usize::MAX);

        assert_eq!(first, EventId::new(EPOCH, 1));
        assert_eq!(second, EventId::new(EPOCH, 2));
        assert_eq!(third, EventId::new(EPOCH, 3));
    }

    #[test]
    fn a_resume_replays_exactly_what_came_after_the_cursor() {
        let mut log = MemoryLog::new(EPOCH);
        let cursor = log.append("team:a:projects", data("1"), 8, usize::MAX);
        log.append("user:b:inbox", data("elsewhere"), 8, usize::MAX);
        log.append("team:a:projects", data("3"), 8, usize::MAX);
        log.append("team:a:projects", data("4"), 8, usize::MAX);

        let opening = open(&log, "team:a:projects", Start::After(cursor));

        assert_eq!(replayed(opening), Some(vec![3, 4]));
    }

    #[test]
    fn a_resume_on_a_quiet_channel_replays_nothing_rather_than_resetting() {
        let mut log = MemoryLog::new(EPOCH);
        let cursor = log.append("team:a:projects", data("1"), 8, usize::MAX);

        let opening = open(&log, "user:never:published", Start::After(cursor));

        assert_eq!(replayed(opening), Some(vec![]));
    }

    #[test]
    fn a_resume_from_before_what_capacity_dropped_is_a_reset() {
        let mut log = MemoryLog::new(EPOCH);
        let cursor = log.append("team:a:projects", data("1"), 2, usize::MAX);
        for text in ["2", "3", "4"] {
            log.append("team:a:projects", data(text), 2, usize::MAX);
        }

        // Event 2 was dropped to keep the buffer at two, and the client never
        // heard it, so nothing short of a reset is honest.
        assert_eq!(
            open(&log, "team:a:projects", Start::After(cursor)),
            Opening::Reset
        );

        // A cursor at the floor itself lost nothing the buffer does not hold.
        let at_floor = EventId::new(EPOCH, 2);
        assert_eq!(
            replayed(open(&log, "team:a:projects", Start::After(at_floor))),
            Some(vec![3, 4]),
        );
    }

    #[test]
    fn the_budget_lets_go_of_the_least_recently_published_channel_whole() {
        let mut log = MemoryLog::new(EPOCH);
        let entry = 10;
        let one_channel = BUFFER_OVERHEAD + entry + ENTRY_OVERHEAD;
        // Room for two channels holding one event each, and not for three.
        let budget = one_channel * 2;

        // A client that connected before anything was published.
        let cursor = EventId::new(EPOCH, 0);
        let heard_the_first = log.append("team:old:projects", data(&"o".repeat(entry)), 8, budget);
        log.append("team:mid:projects", data(&"m".repeat(entry)), 8, budget);
        log.append("team:new:projects", data(&"n".repeat(entry)), 8, budget);

        // The oldest channel is gone, and so its events cannot be vouched for.
        assert_eq!(
            open(&log, "team:old:projects", Start::After(cursor)),
            Opening::Reset
        );
        // Any other channel without a buffer is in the same position.
        assert_eq!(
            open(&log, "user:quiet:inbox", Start::After(cursor)),
            Opening::Reset
        );
        // A cursor at or past everything that was let go resumes exactly.
        assert_eq!(
            replayed(open(
                &log,
                "team:old:projects",
                Start::After(heard_the_first)
            )),
            Some(vec![]),
        );
        assert_eq!(
            replayed(open(
                &log,
                "team:new:projects",
                Start::After(heard_the_first)
            )),
            Some(vec![3]),
        );
        assert_eq!(
            replayed(open(
                &log,
                "user:quiet:inbox",
                Start::After(heard_the_first)
            )),
            Some(vec![]),
        );
    }

    #[test]
    fn a_buffer_reborn_after_eviction_does_not_vouch_for_its_old_events() {
        let mut log = MemoryLog::new(EPOCH);
        let one_channel = BUFFER_OVERHEAD + 1 + ENTRY_OVERHEAD;
        let budget = one_channel;

        let cursor = EventId::new(EPOCH, 0);
        log.append("team:a:projects", data("1"), 8, budget);
        // Publishing elsewhere evicts the first channel whole.
        log.append("team:b:projects", data("2"), 8, budget);
        // And publishing to it again makes it a new buffer.
        log.append("team:a:projects", data("3"), 8, budget);

        // Event 1 is gone even though the channel has a buffer again.
        assert_eq!(
            open(&log, "team:a:projects", Start::After(cursor)),
            Opening::Reset
        );
    }

    #[test]
    fn a_cursor_this_log_never_minted_is_a_reset() {
        let mut log = MemoryLog::new(EPOCH);
        log.append("team:a:projects", data("1"), 8, usize::MAX);

        let other_epoch = EventId::new(EPOCH + 1, 1);
        assert_eq!(
            open(&log, "team:a:projects", Start::After(other_epoch)),
            Opening::Reset
        );

        let from_the_future = EventId::new(EPOCH, 99);
        assert_eq!(
            open(&log, "team:a:projects", Start::After(from_the_future)),
            Opening::Reset
        );

        assert_eq!(
            open(&log, "team:a:projects", Start::Unknown),
            Opening::Reset
        );
    }

    #[test]
    fn the_head_resets_and_a_backfill_replays_everything_kept() {
        let mut log = MemoryLog::new(EPOCH);
        for text in ["1", "2", "3"] {
            log.append("resource:r:logs", data(text), 2, usize::MAX);
        }

        assert_eq!(open(&log, "resource:r:logs", Start::Head), Opening::Reset);
        assert_eq!(
            replayed(open(&log, "resource:r:logs", Start::Backfill)),
            Some(vec![2, 3]),
        );
        assert_eq!(
            replayed(open(&log, "resource:none:logs", Start::Backfill)),
            Some(vec![]),
        );
    }

    #[test]
    fn the_snapshot_names_the_head_it_was_taken_at() {
        let mut log = MemoryLog::new(EPOCH);
        log.append("team:a:projects", data("1"), 8, usize::MAX);
        log.append("team:b:projects", data("2"), 8, usize::MAX);

        let snapshot = log.snapshot([("team:a:projects", None)]);

        assert_eq!(snapshot.epoch, EPOCH);
        assert_eq!(snapshot.head, 2);
        assert!(snapshot.buffers[0].exists);
        assert!(
            snapshot.buffers[0].entries.is_empty(),
            "nothing was asked for"
        );
    }
}
