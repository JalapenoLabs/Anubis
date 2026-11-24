//! The per-process registry of open streams and the channels they carry.
//!
//! Every stream, whichever backend is in use, is served from here: the
//! in-process backend dispatches into it as it publishes, and the Redis
//! backend dispatches into it from the pump that reads Redis. One delivery
//! path means a stream behaves identically in both deployments.
//!
//! Each stream has one queue, however many channels it carries, and every
//! event reaches it in the order of the sequence. That is what makes a
//! stream's cursor honest: the last id a client heard means every earlier
//! event on its channels was delivered too. A stream that falls behind is cut
//! rather than skipped past, so its client reconnects and resumes from the
//! replay log instead of silently missing what was dropped.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, watch};

use super::id::EventId;
use super::remote::Interest;

/// How many channels one process will hold streams for.
///
/// Each entry is a map key and a short list of streams, so the bound costs a
/// few megabytes at worst. It exists because channel names come from
/// browsers: without it, a signed-in client could ask for its own team's
/// channels under endless topics and grow the map without limit. Real
/// deployments sit far below it, since a channel exists here only while a
/// stream carries it.
pub(crate) const MAX_CHANNELS: usize = 16_384;

/// How many events may queue for one stream before it is cut.
///
/// Sized for a burst of writes arriving while one connection is briefly
/// blocked. A stream past it is not slowed and its events are not dropped
/// quietly: it is closed, and its client resumes from the replay log, which
/// is cheaper and more honest than buffering without limit on the server.
const BACKLOG: usize = 256;

/// One event on its way to a stream: its id and its rendered frame.
#[derive(Debug, Clone)]
pub(crate) struct Delivery {
    pub(crate) id: EventId,
    pub(crate) data: Arc<str>,
}

/// The channels this process is carrying, and the streams carrying them.
#[derive(Debug)]
pub(crate) struct Registry {
    state: Mutex<State>,
    /// Where changes in what this process carries are announced.
    ///
    /// `None` for the in-process backend, which has nobody to tell and whose
    /// channels are live the moment they are opened.
    interest: Option<UnboundedSender<Interest>>,
}

#[derive(Debug, Default)]
struct State {
    slots: HashMap<String, Slot>,
    listeners: HashMap<u64, Entry>,
    next_listener: u64,
    next_generation: u64,
}

/// One channel: the streams carrying it, and whether it is being delivered.
#[derive(Debug)]
struct Slot {
    listeners: Vec<u64>,
    /// Which opening of this channel the slot is. A channel closed and opened
    /// again is a new generation, so a confirmation meant for the old one can
    /// never mark the new one live.
    generation: u64,
    ready: watch::Sender<bool>,
}

/// One stream's queue, and the channels it holds.
#[derive(Debug)]
struct Entry {
    sender: mpsc::Sender<Delivery>,
    channels: Vec<String>,
}

impl Registry {
    /// Builds a registry, announcing what it carries to `interest`.
    pub(crate) fn new(interest: Option<UnboundedSender<Interest>>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            interest,
        })
    }

    /// Opens one stream's queue on `channels`.
    ///
    /// Returns the [`Listener`], and the channels it was refused because the
    /// process already holds [`MAX_CHANNELS`] and each would have opened one
    /// more. A channel already open costs nothing and is never refused.
    pub(crate) fn listen(self: &Arc<Self>, channels: &[String]) -> (Listener, Vec<String>) {
        let (sender, receiver) = mpsc::channel(BACKLOG);
        let mut guard = self.lock();
        let state = &mut *guard;
        let id = state.next_listener;
        state.next_listener += 1;

        let mut held = Vec::with_capacity(channels.len());
        let mut refused = Vec::new();
        let mut ready = Vec::with_capacity(channels.len());

        for channel in channels {
            if let Some(slot) = state.slots.get_mut(channel) {
                slot.listeners.push(id);
                ready.push(slot.ready.subscribe());
                held.push(channel.clone());
                continue;
            }

            if state.slots.len() >= MAX_CHANNELS {
                tracing::warn!(
                    realtime.channels = state.slots.len(),
                    "refusing a channel: this process already holds {{realtime.channels}} \
                     channels",
                );
                refused.push(channel.clone());
                continue;
            }

            let generation = state.next_generation;
            state.next_generation += 1;
            // In process there is nobody to wait for. Under Redis, the pump
            // marks the slot live once Redis has confirmed the subscription.
            let (ready_sender, ready_receiver) = watch::channel(self.interest.is_none());
            state.slots.insert(
                channel.clone(),
                Slot {
                    listeners: vec![id],
                    generation,
                    ready: ready_sender,
                },
            );
            ready.push(ready_receiver);
            held.push(channel.clone());
            // Announced under the lock, so an opening and a closing of one
            // channel always reach the pump in the order they happened.
            self.announce(Interest::Opened {
                channel: channel.clone(),
                generation,
            });
        }

        state.listeners.insert(
            id,
            Entry {
                sender,
                channels: held,
            },
        );
        drop(guard);

        let listener = Listener {
            registry: Arc::clone(self),
            id,
            receiver,
            ready,
        };
        (listener, refused)
    }

    /// Queues one event for every stream carrying `channel`.
    ///
    /// A channel no stream carries costs one map lookup. A stream whose queue
    /// is full is cut: its queue still drains what it holds, then ends, and
    /// its client resumes from the replay log.
    pub(crate) fn dispatch(&self, channel: &str, delivery: &Delivery) {
        let mut guard = self.lock();
        let state = &mut *guard;
        let Some(slot) = state.slots.get(channel) else {
            return;
        };

        let mut behind = Vec::new();
        for id in &slot.listeners {
            let Some(entry) = state.listeners.get(id) else {
                continue;
            };
            if entry.sender.try_send(delivery.clone()).is_err() {
                behind.push(*id);
            }
        }

        for id in behind {
            tracing::warn!(
                realtime.channel = channel,
                realtime.backlog = BACKLOG,
                "a realtime stream on {{realtime.channel}} fell {{realtime.backlog}} events \
                 behind and was closed; its client resumes from replay",
            );
            self.cut(state, id);
        }
    }

    /// Marks `channel` live, if it is still the opening `generation` names.
    pub(crate) fn mark_ready(&self, channel: &str, generation: u64) {
        let state = self.lock();
        let Some(slot) = state.slots.get(channel) else {
            return;
        };
        if slot.generation == generation {
            slot.ready.send_replace(true);
        }
    }

    /// Ends every stream this process holds.
    ///
    /// For a delivery path that broke, such as a lost Redis subscriber
    /// connection: whatever was published while it was down never reached
    /// these queues, so the streams end and their clients resume from the
    /// replay log rather than carrying on with a hole in the middle.
    pub(crate) fn interrupt(&self) {
        let mut guard = self.lock();
        let state = &mut *guard;
        let ids: Vec<u64> = state.listeners.keys().copied().collect();
        for id in ids {
            self.cut(state, id);
        }
    }

    /// Closes one stream's queue and releases the channels it held.
    fn release(&self, id: u64) {
        let mut guard = self.lock();
        self.cut(&mut guard, id);
    }

    /// Removes one stream, closing each channel it was the last to hold.
    fn cut(&self, state: &mut State, id: u64) {
        // Dropping the entry drops the only sender, which is what ends the
        // stream's queue once it has drained.
        let Some(entry) = state.listeners.remove(&id) else {
            return;
        };

        for channel in entry.channels {
            let Some(slot) = state.slots.get_mut(&channel) else {
                continue;
            };
            slot.listeners.retain(|listener| *listener != id);
            if !slot.listeners.is_empty() {
                continue;
            }

            let generation = slot.generation;
            state.slots.remove(&channel);
            // Nobody here carries the channel any more, so a remote fanout
            // should stop sending it to this process.
            self.announce(Interest::Closed {
                channel,
                generation,
            });
        }
    }

    /// Tells the fanout backend what this process carries.
    fn announce(&self, interest: Interest) {
        let Some(sender) = &self.interest else {
            return;
        };

        // The receiver only disappears when the pump has stopped, which means
        // the whole `Channels` service is gone and this registry is going too.
        let _sent = sender.send(interest);
    }

    /// Locks the state, recovering what a panicking holder left behind.
    ///
    /// Every invariant the state keeps is a membership list that a panic can
    /// at worst leave one entry stale in, which pins a channel rather than
    /// corrupting anything, so refusing to serve realtime for the rest of the
    /// process would be the larger failure.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many channels the process is holding, for tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().slots.len()
    }
}

/// One stream's queue onto the registry.
///
/// Dropping it releases the channels it held, which is what keeps the
/// registry bounded by open streams: a stream that ends takes its channels
/// with it, even when it ends by being dropped mid-await.
#[derive(Debug)]
pub(crate) struct Listener {
    registry: Arc<Registry>,
    id: u64,
    receiver: mpsc::Receiver<Delivery>,
    ready: Vec<watch::Receiver<bool>>,
}

impl Listener {
    /// Waits until every channel this listener holds is being delivered.
    ///
    /// Immediate in process. Under Redis it waits for Redis to confirm each
    /// subscription, because a snapshot taken before that could miss an event
    /// published in between, one that would then be on neither the snapshot
    /// nor the queue. Returns `false` when a channel was closed while waiting.
    pub(crate) async fn ready(&mut self) -> bool {
        for receiver in &mut self.ready {
            if receiver.wait_for(|ready| *ready).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Waits for the next event, or `None` once the stream has been cut.
    pub(crate) async fn recv(&mut self) -> Option<Delivery> {
        self.receiver.recv().await
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.registry.release(self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BACKLOG, Delivery, Registry};
    use crate::realtime::id::EventId;
    use crate::realtime::remote::Interest;

    fn delivery(sequence: u64) -> Delivery {
        Delivery {
            id: EventId::new(1, sequence),
            data: Arc::from(format!("{{\"sequence\":{sequence}}}")),
        }
    }

    fn names(channels: &[&str]) -> Vec<String> {
        channels
            .iter()
            .map(|channel| (*channel).to_owned())
            .collect()
    }

    #[tokio::test]
    async fn one_queue_carries_every_channel_in_dispatch_order() {
        let registry = Registry::new(None);
        let (mut listener, refused) = registry.listen(&names(&["team:a:x", "user:b:y"]));
        assert!(refused.is_empty());

        registry.dispatch("team:a:x", &delivery(1));
        registry.dispatch("user:b:y", &delivery(2));
        registry.dispatch("team:a:x", &delivery(3));

        for expected in [1, 2, 3] {
            let received = listener.recv().await.expect("the queue is open");
            assert_eq!(received.id.sequence(), expected);
        }
    }

    #[tokio::test]
    async fn every_stream_on_a_channel_receives_a_dispatch() {
        let registry = Registry::new(None);
        let (mut first, _refused) = registry.listen(&names(&["team:a:x"]));
        let (mut second, _refused) = registry.listen(&names(&["team:a:x"]));

        registry.dispatch("team:a:x", &delivery(1));

        for listener in [&mut first, &mut second] {
            let received = listener.recv().await.expect("the queue is open");
            assert_eq!(received.id.sequence(), 1);
        }
    }

    #[tokio::test]
    async fn a_channel_lives_exactly_as_long_as_its_streams() {
        let registry = Registry::new(None);
        let (first, _refused) = registry.listen(&names(&["user:a:inbox"]));
        let (second, _refused) = registry.listen(&names(&["user:a:inbox"]));
        assert_eq!(registry.len(), 1);

        drop(first);
        assert_eq!(registry.len(), 1, "one stream still carries it");

        drop(second);
        assert_eq!(registry.len(), 0, "the last stream closes the channel");
    }

    #[tokio::test]
    async fn interest_is_announced_once_per_opening_in_each_direction() {
        let (sender, mut interest) = tokio::sync::mpsc::unbounded_channel();
        let registry = Registry::new(Some(sender));

        let (first, _refused) = registry.listen(&names(&["team:a:x"]));
        let (second, _refused) = registry.listen(&names(&["team:a:x"]));
        let opened = interest
            .try_recv()
            .expect("the first stream opens the channel");
        let Interest::Opened {
            channel,
            generation,
        } = opened
        else {
            panic!("expected an opening, got {opened:?}");
        };
        assert_eq!(channel, "team:a:x");
        assert!(
            interest.try_recv().is_err(),
            "the second stream is already covered"
        );

        drop(first);
        assert!(interest.try_recv().is_err(), "a stream still carries it");
        drop(second);
        assert_eq!(
            interest.try_recv().ok(),
            Some(Interest::Closed {
                channel: "team:a:x".to_owned(),
                generation,
            }),
        );

        // Opening it again is a new generation.
        let (_third, _refused) = registry.listen(&names(&["team:a:x"]));
        let reopened = interest.try_recv().expect("the channel opens again");
        assert!(
            matches!(reopened, Interest::Opened { generation: next, .. } if next != generation),
            "got {reopened:?}",
        );
    }

    #[tokio::test]
    async fn a_remote_channel_is_live_only_once_its_own_opening_is_confirmed() {
        let (sender, mut interest) = tokio::sync::mpsc::unbounded_channel();
        let registry = Registry::new(Some(sender));

        let (old, _refused) = registry.listen(&names(&["team:a:x"]));
        let Some(Interest::Opened {
            generation: stale, ..
        }) = interest.try_recv().ok()
        else {
            panic!("the channel must open");
        };
        drop(old);

        let (mut listener, _refused) = registry.listen(&names(&["team:a:x"]));
        let _closed = interest.try_recv();
        let Some(Interest::Opened { generation, .. }) = interest.try_recv().ok() else {
            panic!("the channel must open again");
        };

        // A confirmation for the earlier opening is not one for this one.
        registry.mark_ready("team:a:x", stale);
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.ready()).await;
        assert!(
            waited.is_err(),
            "a stale confirmation must not mark it live"
        );

        registry.mark_ready("team:a:x", generation);
        assert!(listener.ready().await);
    }

    #[tokio::test]
    async fn a_stream_that_falls_behind_is_cut_after_draining_what_it_holds() {
        let registry = Registry::new(None);
        let (mut slow, _refused) = registry.listen(&names(&["team:a:x"]));
        let (mut keeping_up, _refused) = registry.listen(&names(&["team:a:x"]));

        let total = u64::try_from(BACKLOG).expect("the backlog fits") + 1;
        for sequence in 1..=total {
            registry.dispatch("team:a:x", &delivery(sequence));
            if let Some(received) = keeping_up.recv().await {
                assert_eq!(received.id.sequence(), sequence);
            }
        }

        // Everything queued before the cut arrives, in order, and then the
        // queue ends so the stream can close and its client resume.
        for expected in 1..total {
            let received = slow.recv().await.expect("the queue drains before ending");
            assert_eq!(received.id.sequence(), expected);
        }
        assert!(slow.recv().await.is_none(), "a cut queue ends");

        // The stream that kept up still carries the channel.
        registry.dispatch("team:a:x", &delivery(total + 1));
        assert!(keeping_up.recv().await.is_some());
    }

    #[tokio::test]
    async fn an_interruption_ends_every_stream() {
        let registry = Registry::new(None);
        let (mut first, _refused) = registry.listen(&names(&["team:a:x"]));
        let (mut second, _refused) = registry.listen(&names(&["user:b:y"]));

        registry.interrupt();

        assert!(first.recv().await.is_none());
        assert!(second.recv().await.is_none());
        assert_eq!(registry.len(), 0);
    }
}
