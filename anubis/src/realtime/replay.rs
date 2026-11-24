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
//! The in-process backend keeps its buffers in a [`MemoryLog`], whose methods
//! state the rules. The Redis backend keeps them in Redis, under two scripts
//! kept in step with those methods: one appends with the same arithmetic, and
//! one decides each opening by the same rules. Both answer a [`Snapshot`], and
//! both decide how a channel opens before reading any of it, so a stream reads
//! only what it sends.

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
pub(crate) struct Start {
    /// What the client says it already heard.
    pub(crate) cursor: Cursor,
    /// Whether the stream's events are the data itself, such as a log tail,
    /// so that a channel opening without an exact replay opens with whatever
    /// it still keeps rather than with nothing.
    pub(crate) backfill: bool,
}

/// What a client says it already heard on one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// Nothing: the channel starts at the head of the sequence.
    Fresh,
    /// Everything up to and including this id.
    After(EventId),
    /// An id this server cannot read.
    Unknown,
}

/// One kept event: its place in the sequence and its rendered frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) sequence: u64,
    pub(crate) data: Arc<str>,
}

/// How some channels' streams open, decided at one instant.
#[derive(Debug)]
pub(crate) struct Snapshot {
    /// The run of the sequence the log is on.
    pub(crate) epoch: u64,
    /// The newest sequence minted so far; a stream is live from here.
    pub(crate) head: u64,
    /// One per channel asked for, in the order they were asked for.
    pub(crate) openings: Vec<Opening>,
}

/// How one channel of a new stream opens.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    /// Whether it opens with a reset frame: the client reads the channel's
    /// state again, or clears it, before anything that follows.
    pub(crate) reset: bool,
    /// The events it replays after that, oldest first, which may be none.
    pub(crate) entries: Vec<Entry>,
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

    /// How each `(channel, start)` asked for opens, at this instant.
    pub(crate) fn snapshot<'name>(
        &self,
        requests: impl IntoIterator<Item = (&'name str, Start)>,
    ) -> Snapshot {
        let openings = requests
            .into_iter()
            .map(|(channel, start)| self.opening(channel, start))
            .collect();

        Snapshot {
            epoch: self.epoch,
            head: self.sequence,
            openings,
        }
    }

    /// How a channel that begins at `start` opens.
    ///
    /// The resume rules, which `snapshot.lua` applies the same way under
    /// Redis. A resume the log can vouch for replays exactly what came after
    /// its cursor. Anything else opens with a reset, followed, for a backfill
    /// stream, by everything the channel keeps; a backfill stream's first
    /// connection opens with what is kept and no reset, since it has nothing
    /// before it to clear. Only the entries the opening sends are read.
    fn opening(&self, channel: &str, start: Start) -> Opening {
        let buffer = self.buffers.get(channel);
        if let Cursor::After(cursor) = start.cursor
            && self.vouches_for(cursor, buffer)
        {
            return Opening {
                reset: false,
                entries: kept_after(buffer, cursor.sequence()),
            };
        }
        if start.backfill {
            return Opening {
                reset: start.cursor != Cursor::Fresh,
                entries: kept_after(buffer, 0),
            };
        }
        Opening {
            reset: true,
            entries: Vec::new(),
        }
    }

    /// Whether every event on a channel after `cursor` is still kept.
    ///
    /// A cursor from another epoch, or from past the head, is not one this
    /// log minted. Past that, it is vouched for unless something after it was
    /// let go: by the channel's capacity, which its buffer's floor records,
    /// or, for a channel with no buffer, by the budget, which the watermark
    /// records.
    fn vouches_for(&self, cursor: EventId, buffer: Option<&Buffer>) -> bool {
        if cursor.epoch() != self.epoch || cursor.sequence() > self.sequence {
            return false;
        }
        let floor = buffer.map_or(self.watermark, |buffer| buffer.floor);
        floor <= cursor.sequence()
    }
}

/// The events a buffer keeps after `since`, oldest first; none for no buffer.
fn kept_after(buffer: Option<&Buffer>, since: u64) -> Vec<Entry> {
    let Some(buffer) = buffer else {
        return Vec::new();
    };
    // Entries are in sequence order, so everything after `since` is one
    // contiguous run at the back.
    let first = buffer
        .entries
        .partition_point(|entry| entry.sequence <= since);
    buffer.entries.range(first..).cloned().collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BUFFER_OVERHEAD, Cursor, ENTRY_OVERHEAD, MemoryLog, Opening, Start};
    use crate::realtime::id::EventId;

    const EPOCH: u64 = 0xfeed;

    /// The reset a stream that is not a backfill opens with: no entries.
    const RESET: Opening = Opening {
        reset: true,
        entries: Vec::new(),
    };

    fn data(text: &str) -> Arc<str> {
        Arc::from(text)
    }

    /// Where a channel begins, for a stream that is not a backfill.
    fn start(cursor: Cursor) -> Start {
        Start {
            cursor,
            backfill: false,
        }
    }

    /// Where a channel begins, for a backfill stream.
    fn backfill(cursor: Cursor) -> Start {
        Start {
            cursor,
            backfill: true,
        }
    }

    /// Resuming after `id`, for a stream that is not a backfill.
    fn after(id: EventId) -> Start {
        start(Cursor::After(id))
    }

    /// How `channel` opens for a stream beginning at `start`.
    fn open(log: &MemoryLog, channel: &str, start: Start) -> Opening {
        log.snapshot([(channel, start)]).openings.remove(0)
    }

    /// The sequences a replay carries, or `None` for a reset.
    fn replayed(opening: Opening) -> Option<Vec<u64>> {
        if opening.reset {
            return None;
        }
        Some(
            opening
                .entries
                .into_iter()
                .map(|entry| entry.sequence)
                .collect(),
        )
    }

    /// The sequences an opening carries, and whether it reset first.
    fn opened(opening: Opening) -> (bool, Vec<u64>) {
        let sequences = opening
            .entries
            .into_iter()
            .map(|entry| entry.sequence)
            .collect();
        (opening.reset, sequences)
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

        let opening = open(&log, "team:a:projects", after(cursor));

        assert_eq!(replayed(opening), Some(vec![3, 4]));
    }

    #[test]
    fn a_resume_on_a_quiet_channel_replays_nothing_rather_than_resetting() {
        let mut log = MemoryLog::new(EPOCH);
        let cursor = log.append("team:a:projects", data("1"), 8, usize::MAX);

        let opening = open(&log, "user:never:published", after(cursor));

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
        assert_eq!(open(&log, "team:a:projects", after(cursor)), RESET);

        // A cursor at the floor itself lost nothing the buffer does not hold.
        let at_floor = EventId::new(EPOCH, 2);
        assert_eq!(
            replayed(open(&log, "team:a:projects", after(at_floor))),
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
        assert_eq!(open(&log, "team:old:projects", after(cursor)), RESET);
        // Any other channel without a buffer is in the same position.
        assert_eq!(open(&log, "user:quiet:inbox", after(cursor)), RESET);
        // A cursor at or past everything that was let go resumes exactly.
        assert_eq!(
            replayed(open(&log, "team:old:projects", after(heard_the_first))),
            Some(vec![]),
        );
        assert_eq!(
            replayed(open(&log, "team:new:projects", after(heard_the_first))),
            Some(vec![3]),
        );
        assert_eq!(
            replayed(open(&log, "user:quiet:inbox", after(heard_the_first))),
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
        assert_eq!(open(&log, "team:a:projects", after(cursor)), RESET);
    }

    #[test]
    fn a_cursor_this_log_never_minted_is_a_reset() {
        let mut log = MemoryLog::new(EPOCH);
        log.append("team:a:projects", data("1"), 8, usize::MAX);

        let other_epoch = EventId::new(EPOCH + 1, 1);
        assert_eq!(open(&log, "team:a:projects", after(other_epoch)), RESET);

        let from_the_future = EventId::new(EPOCH, 99);
        assert_eq!(open(&log, "team:a:projects", after(from_the_future)), RESET);

        assert_eq!(open(&log, "team:a:projects", start(Cursor::Unknown)), RESET);
    }

    #[test]
    fn the_head_resets_and_a_backfill_replays_everything_kept() {
        let mut log = MemoryLog::new(EPOCH);
        for text in ["1", "2", "3"] {
            log.append("resource:r:logs", data(text), 2, usize::MAX);
        }

        assert_eq!(open(&log, "resource:r:logs", start(Cursor::Fresh)), RESET);
        assert_eq!(
            opened(open(&log, "resource:r:logs", backfill(Cursor::Fresh))),
            (false, vec![2, 3]),
        );
        assert_eq!(
            opened(open(&log, "resource:none:logs", backfill(Cursor::Fresh))),
            (false, vec![]),
        );
    }

    #[test]
    fn a_backfill_that_cannot_resume_exactly_resets_then_refills() {
        let mut log = MemoryLog::new(EPOCH);
        let cursor = log.append("resource:r:logs", data("1"), 2, usize::MAX);
        for text in ["2", "3", "4"] {
            log.append("resource:r:logs", data(text), 2, usize::MAX);
        }

        // The cursor is below the floor: the tail clears what it shows, then
        // fills again from the lines still kept, rather than going blank.
        assert_eq!(
            opened(open(
                &log,
                "resource:r:logs",
                backfill(Cursor::After(cursor))
            )),
            (true, vec![3, 4]),
        );
        assert_eq!(
            opened(open(&log, "resource:r:logs", backfill(Cursor::Unknown))),
            (true, vec![3, 4]),
        );
        let foreign = EventId::new(EPOCH + 1, 1);
        assert_eq!(
            opened(open(
                &log,
                "resource:r:logs",
                backfill(Cursor::After(foreign))
            )),
            (true, vec![3, 4]),
        );

        // An exact resume replays only what came after the cursor.
        let recent = EventId::new(EPOCH, 3);
        assert_eq!(
            opened(open(
                &log,
                "resource:r:logs",
                backfill(Cursor::After(recent))
            )),
            (false, vec![4]),
        );
    }

    #[test]
    fn the_snapshot_names_the_head_it_was_taken_at() {
        let mut log = MemoryLog::new(EPOCH);
        log.append("team:a:projects", data("1"), 8, usize::MAX);
        log.append("team:b:projects", data("2"), 8, usize::MAX);

        let snapshot = log.snapshot([("team:a:projects", start(Cursor::Fresh))]);

        assert_eq!(snapshot.epoch, EPOCH);
        assert_eq!(snapshot.head, 2);
        assert_eq!(snapshot.openings, vec![RESET]);
    }
}
