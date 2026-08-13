//! The per-process registry of live channels and their subscribers.
//!
//! Every subscriber, whichever fanout backend is in use, is served from here:
//! the in-process backend dispatches into it directly, and the Redis backend
//! dispatches into it from the pump that reads Redis. One delivery path means
//! a socket behaves identically in both deployments.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

use super::channel::ChannelName;
use super::fanout::Interest;

/// How many channels one process will hold subscribers for.
///
/// Each entry is a small map key and a broadcast sender, so the bound costs a
/// few megabytes at worst. It exists because channel names come from browsers:
/// without it, a signed-in client could subscribe to its own team's channels
/// under endless topics and grow the map without limit. Real deployments sit
/// far below it, since a channel exists only while somebody is listening.
pub(crate) const MAX_CHANNELS: usize = 16_384;

/// How many events a subscriber may fall behind before it loses some.
///
/// Realtime is a notification bus, not a durable log: a client that cannot
/// keep up is told it fell behind and is expected to refetch, which is cheaper
/// and more honest than buffering without limit on the server. Sized for a
/// burst of writes arriving while one socket is briefly blocked.
const BACKLOG: usize = 64;

/// What a subscriber receives.
#[derive(Debug, Clone)]
pub(crate) enum Delivery {
    /// One rendered event frame, ready to forward to a browser verbatim.
    Event(Arc<str>),
    /// The subscriber fell behind and missed this many events.
    Lagged(u64),
}

/// The live channels this process is serving.
#[derive(Debug)]
pub(crate) struct Registry {
    channels: Mutex<HashMap<String, Slot>>,
    /// Where changes in what this process is listening to are announced.
    ///
    /// `None` for the in-process backend, which has nobody to tell.
    interest: Option<UnboundedSender<Interest>>,
}

/// One channel: its subscribers, and how many of them there are.
///
/// The count is kept beside the sender rather than read from
/// `broadcast::Sender::receiver_count`, because a slot must survive the moment
/// between a subscriber being handed a receiver and that receiver being polled.
#[derive(Debug)]
struct Slot {
    sender: broadcast::Sender<Arc<str>>,
    subscribers: usize,
}

impl Registry {
    /// Builds a registry, announcing interest changes to `interest`.
    pub(crate) fn new(interest: Option<UnboundedSender<Interest>>) -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            interest,
        })
    }

    /// Subscribes to `channel` until the returned [`Subscription`] is dropped.
    ///
    /// Returns `None` when the process already holds [`MAX_CHANNELS`] channels
    /// and this call would open one more.
    pub(crate) fn subscribe(self: &Arc<Self>, channel: &ChannelName) -> Option<Subscription> {
        let name = channel.to_string();
        let mut channels = self.lock();

        let receiver = if let Some(slot) = channels.get_mut(&name) {
            slot.subscribers += 1;
            slot.sender.subscribe()
        } else {
            if channels.len() >= MAX_CHANNELS {
                tracing::warn!(
                    realtime.channels = channels.len(),
                    "refusing a subscription: this process already holds \
                     {{realtime.channels}} channels",
                );
                return None;
            }

            let (sender, receiver) = broadcast::channel(BACKLOG);
            channels.insert(
                name.clone(),
                Slot {
                    sender,
                    subscribers: 1,
                },
            );
            // First local interest in this channel, so a remote fanout has to
            // start delivering it here.
            self.announce(Interest::Opened(name.clone()));
            receiver
        };
        drop(channels);

        Some(Subscription {
            registry: Arc::clone(self),
            channel: name,
            receiver,
        })
    }

    /// Delivers a rendered event frame to every subscriber of `channel`.
    ///
    /// A channel nobody is listening to costs one map lookup and nothing else,
    /// which is what makes publishing to a quiet channel free.
    pub(crate) fn dispatch(&self, channel: &str, frame: &str) {
        let channels = self.lock();
        let Some(slot) = channels.get(channel) else {
            return;
        };

        // Fails only when every receiver is gone, which the refcount rules out
        // for a slot that is still in the map.
        let _delivered = slot.sender.send(Arc::from(frame));
    }

    /// Drops one subscriber from `channel`, closing it when it was the last.
    fn release(&self, channel: &str) {
        let mut channels = self.lock();
        let Some(slot) = channels.get_mut(channel) else {
            return;
        };

        slot.subscribers -= 1;
        if slot.subscribers > 0 {
            return;
        }

        channels.remove(channel);
        drop(channels);
        // Nobody here is listening any more, so a remote fanout should stop
        // sending this channel to this process.
        self.announce(Interest::Closed(channel.to_owned()));
    }

    /// Tells the fanout backend what this process is listening to.
    fn announce(&self, interest: Interest) {
        let Some(sender) = &self.interest else {
            return;
        };

        // The receiver only disappears when the pump has stopped, which means
        // the whole `Channels` handle is gone and this registry is going too.
        let _sent = sender.send(interest);
    }

    /// Locks the map, recovering the state a panicking subscriber left behind.
    ///
    /// The invariant a poisoned lock would protect is a refcount, and a stale
    /// refcount pins one channel in the map rather than corrupting anything,
    /// so refusing to serve realtime for the rest of the process would be the
    /// larger failure.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How many channels the process is holding, for tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

/// One subscriber's handle on one channel.
///
/// Dropping it releases the channel, which is what keeps the registry bounded
/// by live interest: a socket that goes away takes its channels with it, even
/// if it goes away by being aborted mid-await.
#[derive(Debug)]
pub(crate) struct Subscription {
    registry: Arc<Registry>,
    channel: String,
    receiver: broadcast::Receiver<Arc<str>>,
}

impl Subscription {
    /// Waits for the next delivery, or `None` once the channel is closed.
    pub(crate) async fn recv(&mut self) -> Option<Delivery> {
        match self.receiver.recv().await {
            Ok(frame) => Some(Delivery::Event(frame)),
            Err(broadcast::error::RecvError::Lagged(missed)) => Some(Delivery::Lagged(missed)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.registry.release(&self.channel);
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{Delivery, Registry};
    use crate::realtime::channel::ChannelName;
    use crate::realtime::fanout::Interest;

    #[tokio::test]
    async fn every_subscriber_of_a_channel_receives_a_dispatch() {
        let registry = Registry::new(None);
        let channel = ChannelName::team(Uuid::new_v4(), "projects");

        let mut first = registry.subscribe(&channel).expect("room for a channel");
        let mut second = registry.subscribe(&channel).expect("room for a channel");
        registry.dispatch(&channel.to_string(), "{\"type\":\"event\"}");

        for subscriber in [&mut first, &mut second] {
            match subscriber.recv().await {
                Some(Delivery::Event(frame)) => assert_eq!(&*frame, "{\"type\":\"event\"}"),
                other => panic!("expected an event, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_channel_lives_exactly_as_long_as_its_subscribers() {
        let registry = Registry::new(None);
        let channel = ChannelName::user(Uuid::new_v4(), "inbox");

        let first = registry.subscribe(&channel).expect("room for a channel");
        let second = registry.subscribe(&channel).expect("room for a channel");
        assert_eq!(registry.len(), 1);

        drop(first);
        assert_eq!(registry.len(), 1, "one subscriber is still listening");

        drop(second);
        assert_eq!(registry.len(), 0, "the last subscriber closes the channel");
    }

    #[tokio::test]
    async fn interest_is_announced_once_per_channel_in_each_direction() {
        let (sender, mut interest) = tokio::sync::mpsc::unbounded_channel();
        let registry = Registry::new(Some(sender));
        let channel = ChannelName::team(Uuid::new_v4(), "projects");
        let name = channel.to_string();

        let first = registry.subscribe(&channel).expect("room for a channel");
        let second = registry.subscribe(&channel).expect("room for a channel");
        assert_eq!(
            interest.try_recv().ok(),
            Some(Interest::Opened(name.clone()))
        );
        assert!(
            interest.try_recv().is_err(),
            "a second subscriber is already covered",
        );

        drop(first);
        assert!(interest.try_recv().is_err(), "somebody is still listening");

        drop(second);
        assert_eq!(interest.try_recv().ok(), Some(Interest::Closed(name)));
    }

    #[tokio::test]
    async fn a_publish_to_a_channel_nobody_holds_is_dropped() {
        let registry = Registry::new(None);
        let channel = ChannelName::team(Uuid::new_v4(), "projects");

        // Nothing to assert but that this neither panics nor allocates a slot.
        registry.dispatch(&channel.to_string(), "{}");

        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn a_subscriber_that_falls_behind_is_told_rather_than_buffered() {
        let registry = Registry::new(None);
        let channel = ChannelName::user(Uuid::new_v4(), "inbox");
        let name = channel.to_string();

        let mut subscription = registry.subscribe(&channel).expect("room for a channel");
        for index in 0..super::BACKLOG + 5 {
            registry.dispatch(&name, &format!("{{\"index\":{index}}}"));
        }

        match subscription.recv().await {
            Some(Delivery::Lagged(missed)) => assert_eq!(missed, 5),
            other => panic!("expected a lag report, got {other:?}"),
        }
    }
}
