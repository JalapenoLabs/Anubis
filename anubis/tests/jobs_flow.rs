//! End-to-end background job flow against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes, so
//! plain `cargo test` still works on machines without a database. CI always
//! provides one.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anubis::jobs::{self, BoxError, Job, Worker};
use anubis::schema::{dead_jobs, jobs as jobs_table};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};

/// A queue of this test's own, so its worker never claims another test's work
/// and no other worker claims its own.
const TEST_QUEUE: &str = "jobs_flow_test";

/// A job that succeeds, recording what it was given.
#[derive(Serialize, Deserialize)]
struct Greet {
    name: String,
}

impl Job for Greet {
    const KIND: &'static str = "jobs_flow_greet";
    const QUEUE: &'static str = TEST_QUEUE;
}

/// A job that fails with attempts to spare, so it is retried.
#[derive(Serialize, Deserialize)]
struct Flaky;

impl Job for Flaky {
    const KIND: &'static str = "jobs_flow_flaky";
    const QUEUE: &'static str = TEST_QUEUE;
    const MAX_ATTEMPTS: i32 = 2;
}

/// A job whose single attempt fails, so it is dead-lettered at once.
#[derive(Serialize, Deserialize)]
struct Doomed;

impl Job for Doomed {
    const KIND: &'static str = "jobs_flow_doomed";
    const QUEUE: &'static str = TEST_QUEUE;
    const MAX_ATTEMPTS: i32 = 1;
}

/// A job on the served queue that the worker has no handler for.
#[derive(Serialize, Deserialize)]
struct Unhandled;

impl Job for Unhandled {
    const KIND: &'static str = "jobs_flow_unhandled";
    const QUEUE: &'static str = TEST_QUEUE;
}

/// A job scheduled far enough out that this run must never see it.
#[derive(Serialize, Deserialize)]
struct Later;

impl Job for Later {
    const KIND: &'static str = "jobs_flow_later";
    const QUEUE: &'static str = TEST_QUEUE;
}

/// A job row still waiting in the queue.
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = jobs_table, check_for_backend(diesel::pg::Pg))]
struct PendingJob {
    attempts: i32,
    run_at: DateTime<Utc>,
    last_error: Option<String>,
}

/// A job row that exhausted its attempts.
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = dead_jobs, check_for_backend(diesel::pg::Pg))]
struct BuriedJob {
    attempts: i32,
    last_error: Option<String>,
}

async fn pending(connection: &mut AsyncPgConnection, kind: &str) -> Option<PendingJob> {
    jobs_table::table
        .filter(jobs_table::kind.eq(kind))
        .select(PendingJob::as_select())
        .first(connection)
        .await
        .optional()
        .expect("the pending job query must succeed")
}

async fn buried(connection: &mut AsyncPgConnection, kind: &str) -> Option<BuriedJob> {
    dead_jobs::table
        .filter(dead_jobs::kind.eq(kind))
        .select(BuriedJob::as_select())
        .first(connection)
        .await
        .optional()
        .expect("the dead job query must succeed")
}

/// Clears this queue, so rows left by an earlier run of the same test against
/// the same database cannot be claimed by this one.
async fn reset(connection: &mut AsyncPgConnection) {
    diesel::delete(jobs_table::table.filter(jobs_table::queue.eq(TEST_QUEUE)))
        .execute(connection)
        .await
        .expect("clearing the test queue must succeed");
    diesel::delete(dead_jobs::table.filter(dead_jobs::queue.eq(TEST_QUEUE)))
        .execute(connection)
        .await
        .expect("clearing the test graveyard must succeed");
}

/// A handler failure, since a closure's error type comes from what it returns.
fn failure(message: &'static str) -> BoxError {
    message.into()
}

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear narrative: a single worker run is what every assertion observes"
)]
async fn jobs_commit_with_their_transaction_then_run_retry_and_die() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping jobs_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must run");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("the database must be reachable");
    let mut connection = pool.get().await.expect("a connection must be available");

    reset(&mut connection).await;

    // A job enqueued in a transaction that rolls back never existed.
    connection
        .transaction::<(), diesel::result::Error, _>(async |connection| {
            jobs::enqueue(
                connection,
                &Greet {
                    name: "rolled back".to_owned(),
                },
            )
            .await
            .expect("enqueueing must succeed");
            Err(diesel::result::Error::RollbackTransaction)
        })
        .await
        .expect_err("the transaction must roll back");
    assert!(
        pending(&mut connection, Greet::KIND).await.is_none(),
        "a rolled-back write must take its job with it",
    );

    jobs::enqueue(
        &mut connection,
        &Greet {
            name: "ada".to_owned(),
        },
    )
    .await
    .expect("enqueueing must succeed");
    jobs::enqueue(&mut connection, &Flaky)
        .await
        .expect("enqueueing must succeed");
    jobs::enqueue(&mut connection, &Doomed)
        .await
        .expect("enqueueing must succeed");
    jobs::enqueue(&mut connection, &Unhandled)
        .await
        .expect("enqueueing must succeed");
    jobs::enqueue_in(&mut connection, &Later, Duration::from_hours(1))
        .await
        .expect("enqueueing must succeed");

    // Every handler that runs records itself here, so the transcript proves
    // both what ran and what did not.
    let transcript = Arc::new(Mutex::new(Vec::new()));

    let worker = Worker::builder(pool.clone())
        .register({
            let transcript = Arc::clone(&transcript);
            move |job: Greet| {
                let transcript = Arc::clone(&transcript);
                async move {
                    unlock(&transcript).push(job.name);
                    Ok(())
                }
            }
        })
        .register(|_: Flaky| async { Err(failure("the dependency is down")) })
        .register(|_: Doomed| async { Err(failure("nothing will fix this")) })
        .register({
            let transcript = Arc::clone(&transcript);
            move |_: Later| {
                let transcript = Arc::clone(&transcript);
                async move {
                    unlock(&transcript).push("later".to_owned());
                    Ok(())
                }
            }
        })
        // Fast enough that the test tracks the worker rather than the clock.
        .poll_interval(Duration::from_millis(25))
        .build();

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(worker.run(async move {
        let _ = stopped.await;
    }));

    // Every immediate job reaches a resting state: run, retried, or buried.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let settled = pending(&mut connection, Greet::KIND).await.is_none()
            && pending(&mut connection, Doomed::KIND).await.is_none()
            && pending(&mut connection, Unhandled::KIND).await.is_none()
            && pending(&mut connection, Flaky::KIND)
                .await
                .is_some_and(|job| job.attempts > 0);
        if settled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never settled the queue",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    stop.send(()).expect("the worker must still be listening");
    running.await.expect("the worker must shut down cleanly");

    assert_eq!(
        *unlock(&transcript),
        vec!["ada".to_owned()],
        "the due job ran, and only the due job",
    );

    // A failure with attempts to spare waits, unlocked, for a later run.
    let flaky = pending(&mut connection, Flaky::KIND)
        .await
        .expect("a retryable job stays in the queue");
    assert_eq!(flaky.attempts, 1);
    assert!(
        flaky.run_at > Utc::now(),
        "a retry must be pushed into the future, not run in a tight loop",
    );
    assert_eq!(
        flaky.last_error.as_deref(),
        Some("the dependency is down"),
        "the failure is kept on the row for debugging",
    );

    // A failure with no attempts left is buried, not dropped.
    assert!(pending(&mut connection, Doomed::KIND).await.is_none());
    let doomed = buried(&mut connection, Doomed::KIND)
        .await
        .expect("an exhausted job must reach the graveyard");
    assert_eq!(doomed.attempts, 1);
    assert_eq!(doomed.last_error.as_deref(), Some("nothing will fix this"));

    // A payload no handler in this build can run is buried too, rather than
    // retried until its attempts run out.
    let unhandled = buried(&mut connection, Unhandled::KIND)
        .await
        .expect("an unroutable job must reach the graveyard");
    assert_eq!(unhandled.attempts, 1);
    assert!(
        unhandled
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("no handler")),
        "the graveyard says why the job could not run",
    );

    // A scheduled job is untouched until its time comes.
    let later = pending(&mut connection, Later::KIND)
        .await
        .expect("a scheduled job waits in the queue");
    assert_eq!(later.attempts, 0);

    reset(&mut connection).await;
}
