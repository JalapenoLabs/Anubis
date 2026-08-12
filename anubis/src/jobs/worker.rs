//! The worker: claims jobs, runs their handlers, and records what happened.
//!
//! One worker owns a registry of handlers keyed by [`Job::KIND`], and draws
//! only from the queues those jobs declare. It claims a batch, runs up to
//! `concurrency` of them at once, and sleeps when the queue is empty. Nothing
//! here holds state a restart would lose: the row is the state.

use std::collections::{BTreeSet, HashMap};
use std::fmt::{self, Debug, Formatter};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeDelta;
use serde_json::Value as JsonValue;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::db::DbPool;
use crate::jobs::queue::{self, Claim, ClaimedJob};
use crate::jobs::{BoxError, Job};

/// Jobs one worker runs at once unless told otherwise.
///
/// Each running job holds a pooled connection only while it records its
/// outcome, so this bounds concurrent handler work rather than connections.
const DEFAULT_CONCURRENCY: usize = 8;

/// How long a worker sleeps before polling a queue it found empty.
///
/// The floor on how late a job starts, so it is short; the queue is one
/// indexed query, so polling it every second costs close to nothing.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long a claim is honored before another worker may reclaim the job.
///
/// The upper bound on how long work stalls when a worker is killed mid-job,
/// and therefore also the longest a handler may run before a sibling worker
/// starts the same job again.
const DEFAULT_LEASE: Duration = Duration::from_mins(5);

/// A registered handler, erased to the payload it decodes.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>;
type Handler = Arc<dyn Fn(JsonValue) -> HandlerFuture + Send + Sync>;

/// Runs background jobs from the Postgres queue.
///
/// Build one with [`Worker::builder`], then drive it with [`Worker::run`].
pub struct Worker {
    pool: DbPool,
    handlers: HashMap<&'static str, Handler>,
    queues: Vec<&'static str>,
    concurrency: usize,
    poll_interval: Duration,
    lease: TimeDelta,
    /// Identifies this worker in the `locked_by` column and in its logs.
    id: String,
}

impl Worker {
    /// Starts building a worker that draws from `pool`.
    #[must_use]
    pub fn builder(pool: DbPool) -> WorkerBuilder {
        WorkerBuilder {
            pool,
            handlers: HashMap::new(),
            queues: BTreeSet::new(),
            concurrency: DEFAULT_CONCURRENCY,
            poll_interval: DEFAULT_POLL_INTERVAL,
            lease: DEFAULT_LEASE,
        }
    }

    /// Runs jobs until `shutdown` resolves, then waits for in-flight jobs.
    ///
    /// Draining is what makes a deploy safe: a job that has started finishes
    /// and records its outcome instead of being reclaimed and run twice. Pass
    /// [`std::future::pending`] to run until the process exits.
    ///
    /// ```ignore
    /// tokio::spawn(worker.run(async {
    ///     let _ = tokio::signal::ctrl_c().await;
    /// }));
    /// ```
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) {
        tracing::info!(
            worker.id = %self.id,
            worker.queues = ?self.queues,
            worker.concurrency = self.concurrency,
            "job worker started on queues {{worker.queues}}",
        );

        tokio::pin!(shutdown);
        let mut in_flight = JoinSet::new();

        loop {
            // Reap finished jobs without blocking, so capacity is current.
            while in_flight.try_join_next().is_some() {}

            let capacity = self.concurrency.saturating_sub(in_flight.len());
            for job in self.claim(capacity).await {
                let handler = self.handlers.get(job.kind.as_str()).map(Arc::clone);
                in_flight.spawn(execute(self.pool.clone(), handler, job));
            }

            // Waiting on a free slot rather than the clock keeps a busy worker
            // claiming the moment it has room, and an idle one off the database.
            let at_capacity = in_flight.len() >= self.concurrency;
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(_finished) = in_flight.join_next(), if !in_flight.is_empty() => {}
                () = tokio::time::sleep(self.poll_interval), if !at_capacity => {}
            }
        }

        tracing::info!(
            worker.id = %self.id,
            worker.in_flight = in_flight.len(),
            "job worker draining {{worker.in_flight}} in-flight jobs",
        );
        while in_flight.join_next().await.is_some() {}
    }

    /// Claims up to `capacity` jobs, treating database trouble as an empty
    /// round: the next poll retries, and one unreachable moment does not end
    /// the worker.
    async fn claim(&self, capacity: usize) -> Vec<ClaimedJob> {
        if capacity == 0 {
            return Vec::new();
        }

        let mut connection = match self.pool.get().await {
            Ok(connection) => connection,
            Err(source) => {
                tracing::error!(
                    worker.id = %self.id,
                    error.message = %source,
                    "the job worker could not reach the database: {{error.message}}",
                );
                return Vec::new();
            }
        };

        let request = Claim {
            queues: &self.queues,
            lease: self.lease,
            worker: &self.id,
            limit: i64::try_from(capacity).unwrap_or(i64::MAX),
        };

        match queue::claim(&mut connection, request).await {
            Ok(claimed) => claimed,
            Err(source) => {
                tracing::error!(
                    worker.id = %self.id,
                    error.message = %source,
                    "the job worker could not claim jobs: {{error.message}}",
                );
                Vec::new()
            }
        }
    }
}

impl Debug for Worker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Worker")
            .field("id", &self.id)
            .field("queues", &self.queues)
            .field("kinds", &self.handlers.keys())
            .field("concurrency", &self.concurrency)
            .field("poll_interval", &self.poll_interval)
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

/// Collects the handlers and knobs a [`Worker`] runs with.
pub struct WorkerBuilder {
    pool: DbPool,
    handlers: HashMap<&'static str, Handler>,
    queues: BTreeSet<&'static str>,
    concurrency: usize,
    poll_interval: Duration,
    lease: Duration,
}

impl WorkerBuilder {
    /// Registers the handler for one job type.
    ///
    /// Annotate the closure's parameter and the job type is inferred. The
    /// closure owns whatever the handler needs, which is all the dependency
    /// wiring a job ever requires:
    ///
    /// ```ignore
    /// Worker::builder(pool)
    ///     .register(move |job: SendWelcomeEmail| {
    ///         let mailer = mailer.clone();
    ///         async move { deliver_welcome(&mailer, job.user_id).await }
    ///     })
    ///     .build()
    /// ```
    ///
    /// Registering a job also subscribes the worker to that job's queue, so a
    /// queue can never be served by a worker that cannot run its jobs.
    ///
    /// # Panics
    /// Panics if two job types share a [`Job::KIND`], which would make a
    /// stored payload's handler ambiguous.
    #[must_use]
    pub fn register<J, F, Fut>(mut self, handler: F) -> Self
    where
        J: Job,
        F: Fn(J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let erased: Handler = Arc::new(move |payload| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let job: J = serde_json::from_value(payload)?;
                handler(job).await
            })
        });

        let clash = self.handlers.insert(J::KIND, erased);
        assert!(
            clash.is_none(),
            "two job types share the kind {:?}",
            J::KIND
        );
        self.queues.insert(J::QUEUE);
        self
    }

    /// Sets how many jobs the worker runs at once.
    ///
    /// # Panics
    /// Panics if `jobs` is zero, which would be a worker that claims nothing.
    #[must_use]
    pub fn concurrency(mut self, jobs: usize) -> Self {
        assert!(jobs > 0, "a worker must be allowed at least one job");
        self.concurrency = jobs;
        self
    }

    /// Sets how long the worker sleeps before polling an empty queue again.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Sets how long a claim is honored before another worker may reclaim it.
    ///
    /// Give it room for the slowest handler registered here: a lease that
    /// expires under a running handler is a second run of the same job.
    #[must_use]
    pub fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Builds the worker.
    #[must_use]
    pub fn build(self) -> Worker {
        Worker {
            pool: self.pool,
            handlers: self.handlers,
            queues: self.queues.into_iter().collect(),
            concurrency: self.concurrency,
            poll_interval: self.poll_interval,
            // Saturates only past the year 262143, where a lease has stopped
            // meaning anything.
            lease: TimeDelta::from_std(self.lease).unwrap_or(TimeDelta::MAX),
            id: Uuid::new_v4().to_string(),
        }
    }
}

impl Debug for WorkerBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerBuilder")
            .field("queues", &self.queues)
            .field("kinds", &self.handlers.keys())
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

/// What running one job produced.
enum Outcome {
    /// The handler returned `Ok`.
    Done,
    /// The handler failed, panicked, or its payload no longer decodes.
    Failed(String),
    /// No handler in this deployment can run this kind.
    Unroutable,
}

/// Runs one claimed job and records what happened to it.
async fn execute(pool: DbPool, handler: Option<Handler>, job: ClaimedJob) {
    let outcome = match handler {
        Some(handler) => invoke(handler, job.payload.clone()).await,
        // The worker only draws from queues it registered jobs for, so an
        // unrecognized kind is not a routing accident: no handler in this
        // build can ever run it. Retrying would hide it; the dead-letter
        // table shows it.
        None => Outcome::Unroutable,
    };

    match record(&pool, &job, &outcome).await {
        Ok(()) => report(&job, &outcome),
        // The job keeps its lease until it expires and then runs again, which
        // is the at-least-once contract handlers are already written for.
        Err(source) => tracing::error!(
            job.id = %job.id,
            job.kind = %job.kind,
            error.message = %source,
            "could not record the outcome of {{job.kind}}: {{error.message}}",
        ),
    }
}

/// Writes an outcome to the job's row: deleted, rescheduled, or buried.
///
/// This takes a connection of its own rather than the handler's, so a job's
/// outcome is never held up by work the handler is still finishing.
async fn record(pool: &DbPool, job: &ClaimedJob, outcome: &Outcome) -> Result<(), BoxError> {
    let mut connection = pool.get().await?;

    match outcome {
        Outcome::Done => queue::complete(&mut connection, job.id).await?,
        Outcome::Failed(message) if job.attempts < job.max_attempts => {
            queue::retry(&mut connection, job.id, message, backoff(job.attempts)).await?;
        }
        Outcome::Failed(message) => queue::dead_letter(&mut connection, job, message).await?,
        Outcome::Unroutable => {
            queue::dead_letter(
                &mut connection,
                job,
                "no handler is registered for this kind",
            )
            .await?;
        }
    }

    Ok(())
}

/// Runs a handler on a task of its own.
///
/// A handler that panics must fail its job, not take down the worker's runner
/// task with it, and a `JoinError` is the only way to observe that panic
/// without unwinding through the worker.
async fn invoke(handler: Handler, payload: JsonValue) -> Outcome {
    match tokio::spawn(handler(payload)).await {
        Ok(Ok(())) => Outcome::Done,
        Ok(Err(source)) => Outcome::Failed(source.to_string()),
        Err(join_error) if join_error.is_panic() => {
            Outcome::Failed("the handler panicked".to_owned())
        }
        Err(_cancelled) => Outcome::Failed("the handler was cancelled".to_owned()),
    }
}

/// Logs what became of a job, at the level its outcome deserves.
fn report(job: &ClaimedJob, outcome: &Outcome) {
    match outcome {
        Outcome::Done => tracing::info!(
            job.id = %job.id,
            job.kind = %job.kind,
            job.attempts = job.attempts,
            "ran {{job.kind}} on attempt {{job.attempts}}",
        ),
        Outcome::Failed(message) if job.attempts < job.max_attempts => tracing::warn!(
            job.id = %job.id,
            job.kind = %job.kind,
            job.attempts = job.attempts,
            job.max_attempts = job.max_attempts,
            error.message = %message,
            "{{job.kind}} failed on attempt {{job.attempts}} of {{job.max_attempts}}: {{error.message}}",
        ),
        Outcome::Failed(message) => tracing::error!(
            job.id = %job.id,
            job.kind = %job.kind,
            job.attempts = job.attempts,
            error.message = %message,
            "{{job.kind}} exhausted {{job.attempts}} attempts and was dead-lettered: {{error.message}}",
        ),
        Outcome::Unroutable => tracing::error!(
            job.id = %job.id,
            job.kind = %job.kind,
            "no handler is registered for {{job.kind}}; the job was dead-lettered",
        ),
    }
}

/// How long a failed job waits before its next attempt.
///
/// Quadratic from a ten second base, capped at an hour: 10s, 40s, 90s, 160s,
/// 250s. A dependency that blips recovers on the first retry; one that is
/// genuinely down is not hammered while it comes back.
fn backoff(attempts: i32) -> TimeDelta {
    /// Seconds the first retry waits, which the curve scales from.
    const BASE_SECONDS: i64 = 10;
    /// Longest a retry ever waits, whatever the attempt count.
    const CAP_SECONDS: i64 = 60 * 60;

    let attempt = i64::from(attempts.max(1));
    let seconds = BASE_SECONDS
        .saturating_mul(attempt)
        .saturating_mul(attempt)
        .min(CAP_SECONDS);

    TimeDelta::try_seconds(seconds).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use diesel_async::AsyncPgConnection;
    use diesel_async::pooled_connection::AsyncDieselConnectionManager;
    use diesel_async::pooled_connection::deadpool::Pool;
    use serde::{Deserialize, Serialize};

    use super::{DEFAULT_CONCURRENCY, Worker, backoff};
    use crate::db::DbPool;
    use crate::jobs::{DEFAULT_QUEUE, Job};

    #[derive(Serialize, Deserialize)]
    struct Welcome {
        user: String,
    }

    impl Job for Welcome {
        const KIND: &'static str = "test_welcome";
    }

    #[derive(Serialize, Deserialize)]
    struct Report;

    impl Job for Report {
        const KIND: &'static str = "test_report";
        const QUEUE: &'static str = "reports";
    }

    /// A pool that is never connected: building one performs no I/O, which is
    /// what lets the registry be tested without a database.
    fn offline_pool() -> DbPool {
        let manager =
            AsyncDieselConnectionManager::<AsyncPgConnection>::new("postgres://unreachable/anubis");
        Pool::builder(manager).build().expect("pool must build")
    }

    #[test]
    fn registration_subscribes_the_worker_to_each_jobs_queue() {
        let worker = Worker::builder(offline_pool())
            .register(|_: Welcome| async { Ok(()) })
            .register(|_: Report| async { Ok(()) })
            .build();

        assert_eq!(worker.queues, vec![DEFAULT_QUEUE, "reports"]);
        assert_eq!(worker.handlers.len(), 2);
        assert!(worker.handlers.contains_key(Welcome::KIND));
        assert_eq!(worker.concurrency, DEFAULT_CONCURRENCY);
    }

    #[test]
    #[should_panic(expected = "two job types share the kind")]
    fn registering_one_kind_twice_is_a_bug() {
        let _worker = Worker::builder(offline_pool())
            .register(|_: Welcome| async { Ok(()) })
            .register(|_: Welcome| async { Ok(()) })
            .build();
    }

    #[tokio::test]
    async fn a_registered_handler_receives_its_decoded_payload() {
        let worker = Worker::builder(offline_pool())
            .register(|job: Welcome| async move {
                assert_eq!(job.user, "ada");
                Ok(())
            })
            .build();

        let handler = worker
            .handlers
            .get(Welcome::KIND)
            .expect("the handler must be registered");
        let payload = serde_json::json!({ "user": "ada" });

        handler(payload).await.expect("the handler must succeed");
    }

    #[tokio::test]
    async fn a_payload_that_no_longer_decodes_fails_its_job() {
        let worker = Worker::builder(offline_pool())
            .register(|_: Welcome| async { Ok(()) })
            .build();

        let handler = worker
            .handlers
            .get(Welcome::KIND)
            .expect("the handler must be registered");
        let outdated = serde_json::json!({ "recipient": "ada" });

        assert!(handler(outdated).await.is_err());
    }

    #[test]
    fn backoff_widens_with_each_attempt_and_stops_at_an_hour() {
        assert_eq!(backoff(1).num_seconds(), 10);
        assert_eq!(backoff(2).num_seconds(), 40);
        assert_eq!(backoff(3).num_seconds(), 90);
        assert_eq!(backoff(60).num_seconds(), 3_600);
        assert_eq!(backoff(i32::MAX).num_seconds(), 3_600);
    }
}
