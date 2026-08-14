//! Processing a stored Hypothetical Sender webhook, in the background.
//!
//! The endpoint stores and queues; everything else happens here, one job per
//! received request. That split is what makes handling retryable: a provider
//! stops caring the moment it gets its `200`, so from then on the queue owns
//! the work, with its own backoff and its own dead-letter table.
//!
//! Delivery is at-least-once, like every job on this queue, so [`act_on`] may
//! run twice for one webhook. The `processed_at` stamp is the guard against
//! that, and any handling that is not naturally idempotent needs one of its
//! own.

use anubis::db::DbPool;
use anubis::jobs::{BoxError, Job, WorkerBuilder};
use diesel_async::AsyncPgConnection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::HypotheticalSenderWebhook;

/// The queue incoming webhooks are processed on.
///
/// Its own, rather than the application's default: a provider that replays a
/// day of events, or one whose handling is slow, must not hold up the work a
/// person is waiting on.
pub const QUEUE: &str = "incoming_webhooks";

/// The job that processes one stored webhook.
///
/// The payload is the row's id and nothing else, because the row is the only
/// current view of what arrived by the time a worker reaches it.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessHypotheticalSenderWebhook {
    /// The `hypothetical_sender_webhooks` row to process.
    pub webhook_id: Uuid,
}

impl Job for ProcessHypotheticalSenderWebhook {
    /// Namespaced by the model it belongs to, because a `KIND` is data shared
    /// with every row already enqueued under it.
    const KIND: &'static str = "hypothetical_sender_webhook.process";
    const QUEUE: &'static str = QUEUE;
}

/// Registers this webhook's processing job on the application's worker.
///
/// The closure owns whatever the handler needs, which is all the dependency
/// wiring a job ever requires: a mailer, an HTTP client, or a service of your
/// own goes here beside the pool.
#[must_use]
pub fn register_jobs(pool: &DbPool, worker: WorkerBuilder) -> WorkerBuilder {
    let pool = pool.clone();
    worker.register(move |job: ProcessHypotheticalSenderWebhook| {
        let pool = pool.clone();
        async move { process(pool, job).await }
    })
}

/// Runs one job: load the row, act on it, and stamp what happened.
///
/// A row that is already processed returns without acting, which is what makes
/// a redelivered job harmless.
async fn process(pool: DbPool, job: ProcessHypotheticalSenderWebhook) -> Result<(), BoxError> {
    let mut connection = pool.get().await?;
    let record = HypotheticalSenderWebhook::find(&mut connection, job.webhook_id).await?;
    if record.processed_at.is_some() {
        return Ok(());
    }

    match act_on(&mut connection, &record).await {
        Ok(()) => {
            HypotheticalSenderWebhook::mark_processed(&mut connection, record.id).await?;
            Ok(())
        }
        Err(failure) => {
            // Recorded on the row as well as returned: the queue's own error
            // lives with the job and disappears when it succeeds, and the row
            // is what a developer looks at when a provider asks why an event
            // had no effect.
            HypotheticalSenderWebhook::mark_failed(
                &mut connection,
                record.id,
                &failure.to_string(),
            )
            .await?;
            Err(failure)
        }
    }
}

/// Acts on one received webhook.
///
/// **This is the function you fill in.** Everything around it, the storing, the
/// queueing, the retries, and the stamping, is already done.
///
/// What you have to work with:
///
/// - `record.payload` is the provider's JSON, exactly as it arrived. Match on
///   whatever field names the event type, and ignore the types you do not care
///   about; a provider sends more of them than an application ever wants.
/// - `record.verified` says whether the signature checked out. Decide here what
///   an unverified event is worth: usually nothing, so return an error and let
///   it sit in the table as evidence.
/// - `_connection` is a pooled connection, so drop the underscore and write
///   through it. Wrap several writes in one transaction if they belong
///   together; the stamp that marks this row processed is a separate statement
///   on purpose, so a partial handling cannot be recorded as a finished one.
///
/// # Errors
/// Return an error and the queue retries on its own widening backoff, then
/// dead-letters the job. The row keeps the message either way.
#[expect(
    clippy::unused_async,
    reason = "the stub reaches nothing yet; the handling that replaces it will"
)]
async fn act_on(
    _connection: &mut AsyncPgConnection,
    record: &HypotheticalSenderWebhook,
) -> Result<(), BoxError> {
    tracing::info!(
        webhook.id = %record.id,
        webhook.verified = record.verified,
        "received a Hypothetical Sender webhook and did nothing with it: {{webhook.id}}",
    );
    Ok(())
}
