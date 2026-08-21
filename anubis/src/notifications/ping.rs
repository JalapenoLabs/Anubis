//! The realtime half: a job that tells one browser to refetch its inbox.
//!
//! Publishing is not transactional and a notification is, so the publish
//! cannot happen where the row is written: a ping sent inside a transaction
//! that later rolls back tells a browser to refetch a notification that never
//! existed, and one sent before the commit races the refetch it asks for.
//! [`notify`](super::notify) therefore enqueues a [`PingRecipient`] on the
//! caller's connection, exactly as it writes the row, and a [`Pinger`]
//! registered on a worker publishes it once the transaction has committed.
//!
//! Nothing durable rides the socket. The frame carries the recipient and the
//! word `created`, and the browser answers by reading `/account/notifications`
//! again, so a client that was disconnected is never out of step for longer
//! than its next fetch.

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::jobs::{BoxError, Job};
use crate::realtime::{ChannelName, Channels};

/// The queue notification pings run on.
///
/// Named here as well as on the job so an application can give it a worker of
/// its own without repeating a string literal.
pub const QUEUE: &str = "notifications";

/// The topic the recipient's browser listens to, under the `user` audience.
///
/// The full channel name is `user:{user_id}:notifications`; the bell in
/// `@jalapenolabs/anubis` subscribes to exactly this.
pub const TOPIC: &str = "notifications";

/// The event name published when a notification is written.
const EVENT: &str = "created";

/// Tells one recipient's browsers that their inbox changed.
///
/// The payload is the recipient and nothing else: a worker reaching this job
/// has no view of the notification worth trusting, and the browser refetches
/// regardless of what the frame says.
#[derive(Debug, Serialize, Deserialize)]
pub struct PingRecipient {
    /// The user whose channel is published to.
    pub user_id: Uuid,
}

impl Job for PingRecipient {
    /// Namespaced, because a `KIND` is data shared with every row already
    /// enqueued and an application's own job must never collide with it.
    const KIND: &'static str = "anubis.notifications.ping";
    /// Its own queue: a ping is worthless once it is late, and it must not
    /// wait behind an application's slow work.
    const QUEUE: &'static str = QUEUE;
    /// Fewer attempts than a delivery gets. The row is already durable, so a
    /// ping that cannot be published costs a refresh rather than a notice.
    const MAX_ATTEMPTS: i32 = 3;
}

/// Publishes pings: the handler side of [`PingRecipient`].
///
/// Register it on a worker and the framework owns the rest of the path from a
/// write to a bell that moves:
///
/// ```ignore
/// let pinger = anubis::notifications::Pinger::new(channels.clone());
/// let worker = anubis::jobs::Worker::builder(pool)
///     .register(move |job: anubis::notifications::PingRecipient| {
///         let pinger = pinger.clone();
///         async move { pinger.ping(job).await }
///     })
///     .build();
/// ```
///
/// An application that registers no handler for this job still delivers every
/// notification; its bells update on their next fetch instead of at once.
#[derive(Debug, Clone)]
pub struct Pinger {
    channels: Channels,
}

impl Pinger {
    /// Builds a pinger that publishes through `channels`.
    #[must_use]
    pub fn new(channels: Channels) -> Self {
        Self { channels }
    }

    /// Publishes one ping.
    ///
    /// # Errors
    /// Returns the publish failure, so the queue retries on its own backoff.
    /// Publishing to a channel nobody is listening to is not a failure: a
    /// recipient with no open tab has nothing to be told.
    pub async fn ping(&self, job: PingRecipient) -> Result<(), BoxError> {
        self.channels
            .publish(
                &ChannelName::user(job.user_id, TOPIC),
                EVENT,
                // Deliberately empty: the client refetches, and a payload it
                // could render would be a second source of truth for the row.
                json!({}),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{EVENT, PingRecipient, Pinger, QUEUE, TOPIC};
    use crate::jobs::Job;
    use crate::realtime::{ChannelName, Channels};

    #[test]
    fn the_job_is_namespaced_and_runs_on_its_own_queue() {
        assert!(PingRecipient::KIND.starts_with("anubis."));
        assert_eq!(PingRecipient::QUEUE, QUEUE);
        assert_ne!(PingRecipient::QUEUE, crate::jobs::DEFAULT_QUEUE);
    }

    #[test]
    fn the_channel_is_the_one_the_bell_subscribes_to() {
        let user_id = Uuid::new_v4();
        assert_eq!(
            ChannelName::user(user_id, TOPIC).to_string(),
            format!("user:{user_id}:notifications"),
        );
    }

    #[tokio::test]
    async fn a_ping_reaches_the_recipient_and_carries_nothing() {
        let channels = Channels::in_process();
        let user_id = Uuid::new_v4();
        let pinger = Pinger::new(channels.clone());

        pinger
            .ping(PingRecipient { user_id })
            .await
            .expect("publishing in process never fails");

        // Nobody was listening, which is the common case and not a failure.
        // What the frame says is asserted in `notifications_flow`, over a real
        // socket; here the contract is that the event name never drifts.
        assert_eq!(EVENT, "created");
    }
}
