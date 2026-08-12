//! The queue's storage layer: enqueue, claim, complete, retry, dead-letter.
//!
//! Everything here takes a connection rather than a pool, so a caller can run
//! it inside their own transaction. [`claim`] is the only interesting query:
//! it selects candidate rows `FOR UPDATE SKIP LOCKED` and stamps them in the
//! same transaction, which is what lets any number of workers draw from one
//! table without ever handing the same row to two of them.

use chrono::{DateTime, TimeDelta, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::schema::{dead_jobs, jobs};

/// Longest failure message kept on a row.
///
/// Long enough for a message that names the failing call, short enough that a
/// handler echoing a large response body cannot bloat the table.
const MAX_ERROR_LENGTH: usize = 2_000;

/// A job about to be enqueued.
#[derive(Debug, Insertable)]
#[diesel(table_name = jobs)]
pub(super) struct NewJob<'a> {
    pub queue: &'a str,
    pub kind: &'a str,
    pub payload: JsonValue,
    pub max_attempts: i32,
    pub run_at: DateTime<Utc>,
}

/// A job row a worker holds the lease on.
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = jobs, check_for_backend(diesel::pg::Pg))]
pub(super) struct ClaimedJob {
    pub id: Uuid,
    pub queue: String,
    pub kind: String,
    pub payload: JsonValue,
    /// Attempts spent, this run included.
    pub attempts: i32,
    pub max_attempts: i32,
    /// When the job was first enqueued, carried into `dead_jobs`.
    pub created_at: DateTime<Utc>,
}

/// What a worker asks for when it claims a batch.
#[derive(Debug)]
pub(super) struct Claim<'a> {
    /// The queues the worker has handlers for.
    pub queues: &'a [&'static str],
    /// How long this claim is honored before another worker may reclaim it.
    pub lease: TimeDelta,
    /// Identifies the claiming worker in `locked_by`.
    pub worker: &'a str,
    /// Most rows to take in one round.
    pub limit: i64,
}

/// Inserts a job, returning its id.
pub(super) async fn insert(
    connection: &mut AsyncPgConnection,
    job: NewJob<'_>,
) -> QueryResult<Uuid> {
    diesel::insert_into(jobs::table)
        .values(job)
        .returning(jobs::id)
        .get_result(connection)
        .await
}

/// Claims up to `request.limit` runnable jobs for one worker.
///
/// A job is runnable when its queue is served, its `run_at` has passed, and it
/// is either unlocked or locked past the lease. Claiming spends an attempt,
/// so a worker that dies holding a lease cannot retry its job forever.
pub(super) async fn claim(
    connection: &mut AsyncPgConnection,
    request: Claim<'_>,
) -> QueryResult<Vec<ClaimedJob>> {
    connection
        .transaction::<Vec<ClaimedJob>, diesel::result::Error, _>(async |connection| {
            let now = Utc::now();
            let abandoned_before = now - request.lease;

            // Locking the candidates and stamping them in one transaction is
            // what makes the claim atomic; SKIP LOCKED lets sibling workers
            // pass over these rows instead of queueing behind them.
            let claimed: Vec<Uuid> = jobs::table
                .filter(jobs::queue.eq_any(request.queues))
                .filter(jobs::run_at.le(now))
                .filter(
                    jobs::locked_at
                        .is_null()
                        .or(jobs::locked_at.lt(abandoned_before)),
                )
                .order(jobs::run_at.asc())
                .limit(request.limit)
                .select(jobs::id)
                .for_update()
                .skip_locked()
                .load(connection)
                .await?;

            if claimed.is_empty() {
                return Ok(Vec::new());
            }

            diesel::update(jobs::table.filter(jobs::id.eq_any(claimed)))
                .set((
                    jobs::locked_at.eq(now),
                    jobs::locked_by.eq(request.worker),
                    jobs::attempts.eq(jobs::attempts + 1),
                ))
                .returning(ClaimedJob::as_returning())
                .get_results(connection)
                .await
        })
        .await
}

/// Deletes a job that ran successfully.
pub(super) async fn complete(connection: &mut AsyncPgConnection, id: Uuid) -> QueryResult<()> {
    diesel::delete(jobs::table.find(id))
        .execute(connection)
        .await?;

    Ok(())
}

/// Releases a failed job back to the queue, to run again after `backoff`.
pub(super) async fn retry(
    connection: &mut AsyncPgConnection,
    id: Uuid,
    message: &str,
    backoff: TimeDelta,
) -> QueryResult<()> {
    let run_at = Utc::now()
        .checked_add_signed(backoff)
        .unwrap_or(DateTime::<Utc>::MAX_UTC);

    diesel::update(jobs::table.find(id))
        .set((
            jobs::run_at.eq(run_at),
            jobs::locked_at.eq(None::<DateTime<Utc>>),
            jobs::locked_by.eq(None::<String>),
            jobs::last_error.eq(truncate(message)),
        ))
        .execute(connection)
        .await?;

    Ok(())
}

/// Moves an exhausted job into `dead_jobs`, id and history intact.
///
/// The move is one transaction, so a job is never in both tables and never in
/// neither.
pub(super) async fn dead_letter(
    connection: &mut AsyncPgConnection,
    job: &ClaimedJob,
    message: &str,
) -> QueryResult<()> {
    let dead = DeadJob {
        id: job.id,
        queue: &job.queue,
        kind: &job.kind,
        payload: &job.payload,
        attempts: job.attempts,
        last_error: truncate(message),
        enqueued_at: job.created_at,
    };

    connection
        .transaction::<(), diesel::result::Error, _>(async |connection| {
            diesel::insert_into(dead_jobs::table)
                .values(dead)
                .execute(connection)
                .await?;
            diesel::delete(jobs::table.find(job.id))
                .execute(connection)
                .await?;
            Ok(())
        })
        .await
}

#[derive(Debug, Insertable)]
#[diesel(table_name = dead_jobs)]
struct DeadJob<'a> {
    id: Uuid,
    queue: &'a str,
    kind: &'a str,
    payload: &'a JsonValue,
    attempts: i32,
    last_error: String,
    enqueued_at: DateTime<Utc>,
}

/// Caps a failure message at [`MAX_ERROR_LENGTH`], on a character boundary.
fn truncate(message: &str) -> String {
    message.chars().take(MAX_ERROR_LENGTH).collect()
}

#[cfg(test)]
mod tests {
    use super::{MAX_ERROR_LENGTH, truncate};

    #[test]
    fn short_messages_survive_intact() {
        assert_eq!(truncate("connection refused"), "connection refused");
    }

    #[test]
    fn long_messages_are_capped_without_splitting_characters() {
        let message = "🐺".repeat(MAX_ERROR_LENGTH + 10);
        let truncated = truncate(&message);

        assert_eq!(truncated.chars().count(), MAX_ERROR_LENGTH);
        assert!(truncated.starts_with('🐺'));
    }
}
