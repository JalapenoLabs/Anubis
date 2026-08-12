//! Durable background jobs, queued in PostgreSQL.
//!
//! A job is a serializable struct that implements [`Job`]. Enqueueing writes a
//! row through the caller's own connection, which is the whole point of
//! keeping the queue in Postgres: [`enqueue`] inside a transaction commits
//! with the domain write that caused it. A rolled-back write leaves no job
//! behind, and a committed write never loses its follow-up work. Redis stays
//! out of this path entirely.
//!
//! # Defining and enqueueing a job
//!
//! ```ignore
//! use anubis::jobs::{self, Job};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct SendWelcomeEmail {
//!     user_id: Uuid,
//! }
//!
//! impl Job for SendWelcomeEmail {
//!     const KIND: &'static str = "send_welcome_email";
//! }
//!
//! connection
//!     .transaction(async |connection| {
//!         let user = create_user(connection).await?;
//!         jobs::enqueue(connection, &SendWelcomeEmail { user_id: user.id }).await?;
//!         Ok(())
//!     })
//!     .await?;
//! ```
//!
//! # Running jobs
//!
//! A [`Worker`] draws from every queue its registered jobs declare, so
//! registration is the only place a queue is named:
//!
//! ```ignore
//! let worker = Worker::builder(pool)
//!     .register(move |job: SendWelcomeEmail| {
//!         let mailer = mailer.clone();
//!         async move { deliver_welcome(&mailer, job.user_id).await }
//!     })
//!     .build();
//!
//! tokio::spawn(worker.run(async {
//!     let _ = tokio::signal::ctrl_c().await;
//! }));
//! ```
//!
//! # Delivery guarantees
//!
//! Delivery is at-least-once, so **handlers must be idempotent**. A worker
//! claims a row by stamping it with a lease and spending one attempt; if the
//! worker dies mid-job, the lease expires and another worker retries. The
//! process that makes a job durable is the same one that can run it twice, and
//! no queue can have both properties.
//!
//! A handler that returns an error, panics, or receives a payload that no
//! longer decodes fails its job. Failures retry on a widening backoff until
//! the job's [`Job::MAX_ATTEMPTS`] runs out, at which point the job moves to
//! the `dead_jobs` table with its last error, where it waits for an operator
//! rather than disappearing.

mod queue;
mod worker;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diesel_async::AsyncPgConnection;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

#[doc(inline)]
pub use worker::{Worker, WorkerBuilder};

/// The queue a job runs on unless its type names another.
pub const DEFAULT_QUEUE: &str = "default";

/// The error a job handler returns. Any error type converts into it with `?`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A unit of background work: a payload plus the terms it runs under.
///
/// Implement it on a plain serializable struct. The struct is the payload, so
/// carry ids rather than whole records: a job may run minutes after it was
/// enqueued, and the database is the only current view of a record by then.
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable identifier that routes a stored payload back to its handler.
    ///
    /// This is data, not a symbol: renaming it strands every row already
    /// enqueued under the old name.
    const KIND: &'static str;

    /// The queue this job runs on.
    ///
    /// Put slow or failure-prone work on its own queue and give it its own
    /// worker, so it cannot starve everything else.
    const QUEUE: &'static str = DEFAULT_QUEUE;

    /// How many times the job may run before it is dead-lettered.
    ///
    /// Copied onto the row at enqueue time, so raising or lowering it later
    /// never changes the terms in-flight jobs were accepted under.
    const MAX_ATTEMPTS: i32 = 5;
}

/// Enqueues `job` to run as soon as a worker has room for it.
///
/// The insert rides on `connection`, so calling this inside a transaction ties
/// the job to that transaction. Returns the new job's id, which the worker
/// logs as `job.id` while running it.
///
/// # Errors
/// Returns an [`Error`] if the payload fails to serialize or the insert fails.
pub async fn enqueue<J: Job>(connection: &mut AsyncPgConnection, job: &J) -> Result<Uuid, Error> {
    enqueue_in(connection, job, Duration::ZERO).await
}

/// Enqueues `job` to run no earlier than `delay` from now.
///
/// The delay is a floor, not a schedule: the job runs once a worker reaches it
/// after that point. See [`enqueue`] for the transactional guarantee.
///
/// # Errors
/// Returns an [`Error`] if the payload fails to serialize or the insert fails.
pub async fn enqueue_in<J: Job>(
    connection: &mut AsyncPgConnection,
    job: &J,
    delay: Duration,
) -> Result<Uuid, Error> {
    let payload = serde_json::to_value(job)
        .map_err(|source| Error::new("failed to serialize the job payload", source))?;

    // Both conversions only saturate past the year 262143, where the exact
    // instant has stopped meaning anything; a nonsense delay becomes a job
    // that never runs rather than an error the caller has to handle.
    let delay = TimeDelta::from_std(delay).unwrap_or(TimeDelta::MAX);
    let run_at = Utc::now()
        .checked_add_signed(delay)
        .unwrap_or(DateTime::<Utc>::MAX_UTC);

    queue::insert(
        connection,
        queue::NewJob {
            queue: J::QUEUE,
            kind: J::KIND,
            payload,
            max_attempts: J::MAX_ATTEMPTS,
            run_at,
        },
    )
    .await
    .map_err(|source| Error::new("failed to enqueue the job", source))
}

/// A failure to enqueue a job.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: BoxError,
    backtrace: Backtrace,
}

impl Error {
    fn new(context: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            context,
            source: Box::new(source),
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
