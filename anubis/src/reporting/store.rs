//! One row per distinct fault, counted under its own lock.
//!
//! Every occurrence passes through [`record`], which counts it and answers with
//! the one thing left to do: nothing, recheck the open issue, or file one. The
//! answer and the job that carries it out commit together, which is what makes
//! the queue an outbox: a counted occurrence that needs filing can never be
//! left without the job that files it.
//!
//! **Counting is an upsert, not a read followed by a write.** `SELECT ... FOR
//! UPDATE` cannot lock a row that does not exist, so the first burst of a new
//! fault would have two occurrences each find nothing and each file. `ON
//! CONFLICT DO UPDATE` takes the row's lock and counts in one statement, so
//! there is no gap to race in.
//!
//! The policy itself is [`next_step`], a pure function over the row's state and
//! how long since the tracker was last asked about it, which is what makes the
//! rule testable without a database.

use chrono::{DateTime, TimeDelta, Utc};
use diesel::prelude::*;
use diesel::upsert::excluded;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use serde_json::Value as JsonValue;

use super::filing::{Action, FileErrorReport};
use super::{Report, Sample, Source, title};
use crate::jobs;
use crate::schema::error_reports;

/// How long an open issue is trusted before its state is read back.
///
/// Somebody closing an issue is how the application learns a fault was dealt
/// with, and nothing tells it when that happens. An hour is short enough that a
/// regression is filed again the same morning, and long enough that a fault
/// looping all day costs twenty-four calls rather than a rate limit. The
/// recheck is also when an open issue is told how many more times it happened.
///
/// **It must not drop below [`CEILING_WINDOW`].** A row keeps one `filed_at`,
/// so filing again overwrites the one before. That is harmless only while the
/// overwritten filing has already left the window, which this being the longer
/// of the two guarantees. Asserted below.
const RECHECK_AFTER: TimeDelta = TimeDelta::hours(1);

/// How long to wait before retrying a filing that failed or never finished.
///
/// Covers a tracker outage, a refused token, nothing being configured, and a
/// process that died holding the claim. Without it a fault in a fast loop
/// retries on every occurrence, which turns one outage into thousands of calls.
/// It is also far longer than a filing job's own retries take, so a job still
/// retrying is never overtaken by a second filing of the same fault.
const RETRY_AFTER: TimeDelta = TimeDelta::minutes(15);

/// How many issues may be filed in [`CEILING_WINDOW`].
///
/// The guard against a storm of *distinct* fingerprints, which is what a fault
/// carrying something normalization did not strip looks like. Ten is high
/// enough that a genuinely bad morning is fully reported and low enough that a
/// runaway is ten issues rather than a repository nobody can use. A withheld
/// fault is still counted; only the issue is withheld.
pub const MAX_ISSUES_PER_WINDOW: i64 = 10;

/// The window [`MAX_ISSUES_PER_WINDOW`] is counted over.
const CEILING_WINDOW: TimeDelta = TimeDelta::hours(1);

const _: () = assert!(
    RECHECK_AFTER.num_seconds() >= CEILING_WINDOW.num_seconds(),
    "RECHECK_AFTER must be at least CEILING_WINDOW, or overwriting filed_at \
     drops a filing that the ceiling should still be counting"
);

/// The advisory lock every filing decision serializes on.
///
/// Arbitrary, and only has to differ from the other advisory locks taken
/// against this database: the framework's migration lock is `...0001`.
const CEILING_LOCK: i64 = 0x414E_5542_4953_0002;

/// What a withheld row says about why it has no issue.
const WITHHELD: &str = "Withheld: this hour's issue ceiling was already taken.";

/// Where a fingerprint stands with the tracker.
///
/// See the migration's column comment for what each means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// A filing job is queued, or was and its process died.
    Queued,
    /// An issue exists and was open the last time anybody looked.
    Open,
    /// The tracker refused.
    Failed,
    /// The hourly ceiling was taken.
    Withheld,
    /// Nothing is configured to file with.
    Unconfigured,
}

impl State {
    /// The value stored in `error_reports.state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Open => "open",
            Self::Failed => "failed",
            Self::Withheld => "withheld",
            Self::Unconfigured => "unconfigured",
        }
    }

    /// Reads a stored value back. An unknown one reads as `failed`, which is
    /// retried on the backoff rather than trusted as open.
    fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "open" => Self::Open,
            "withheld" => Self::Withheld,
            "unconfigured" => Self::Unconfigured,
            _ => Self::Failed,
        }
    }
}

/// What [`record`] did with an occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Counted against an issue that already covers it, or a filing already
    /// under way. Nothing else to do.
    Counted,
    /// A filing job was queued.
    Queued,
    /// A recheck of the open issue was queued.
    Rechecking,
    /// Counted, and not filed: this hour's ceiling is taken.
    Withheld,
}

/// One fault, as an operator's screen reads it.
#[derive(Debug, Clone, PartialEq, Queryable, Selectable, Serialize)]
#[diesel(table_name = error_reports, check_for_backend(diesel::pg::Pg))]
pub struct ErrorReport {
    pub fingerprint: String,
    pub source: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub issue_number: Option<i64>,
    pub issue_url: Option<String>,
    pub occurrences: i64,
    pub failure: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// Counts an occurrence of `report`, and queues whatever it calls for.
///
/// One transaction: the upsert counts and takes the row's lock, the decision is
/// written, and the job that carries it out is enqueued, all or nothing.
///
/// **Permission to file is granted here, not checked later.** See
/// [`reserve_slot`]: a ceiling a caller reads and then acts on is a ceiling
/// every member of a simultaneous burst passes.
///
/// # Errors
/// Returns the database's error when the occurrence cannot be recorded. A
/// caller treats that as "nothing was filed", since deduplication is exactly
/// what has just failed.
pub async fn record(connection: &mut AsyncPgConnection, report: &Report) -> QueryResult<Outcome> {
    let fingerprint = report.fingerprint();
    let sample = serde_json::to_value(report.sample())
        .map_err(|error| diesel::result::Error::SerializationError(Box::new(error)))?;
    let title = title(report.source(), report.summary());

    connection
        .transaction::<Outcome, diesel::result::Error, _>(async |connection| {
            let row: ClaimRow = diesel::insert_into(error_reports::table)
                .values((
                    error_reports::fingerprint.eq(&fingerprint),
                    error_reports::source.eq(report.source().as_str()),
                    error_reports::kind.eq(report.kind()),
                    error_reports::title.eq(&title),
                    error_reports::sample.eq(&sample),
                ))
                .on_conflict(error_reports::fingerprint)
                .do_update()
                .set((
                    error_reports::occurrences.eq(error_reports::occurrences + 1),
                    error_reports::last_seen_at.eq(diesel::dsl::now),
                    // The latest occurrence is what an issue filed from here
                    // on quotes, and what a recheck's comment shows.
                    error_reports::sample.eq(excluded(error_reports::sample)),
                ))
                .returning(ClaimRow::as_returning())
                .get_result(connection)
                .await?;

            // A fault nobody has seen goes straight to filing. `occurrences` is
            // what says so: an insert takes the default of one and the update
            // only ever increments, so nothing else can carry that value.
            let step = if row.occurrences == 1 {
                Step::File
            } else {
                next_step(State::parse(&row.state), Utc::now() - row.checked_at)
            };

            // An open row with no issue number was never really filed. Filing
            // it is the repair, and routing it through `File` puts it behind
            // the reservation with every other filing.
            let step = if step == Step::Recheck && row.issue_number.is_none() {
                Step::File
            } else {
                step
            };

            match step {
                Step::Count => Ok(Outcome::Counted),
                Step::Recheck => {
                    // Moved now rather than after the call, so an occurrence
                    // arriving while the tracker is asked does not ask again.
                    diesel::update(error_reports::table.find(&fingerprint))
                        .set(error_reports::checked_at.eq(diesel::dsl::now))
                        .execute(connection)
                        .await?;
                    jobs::enqueue_query(
                        connection,
                        &FileErrorReport {
                            fingerprint: fingerprint.clone(),
                            action: Action::Recheck,
                        },
                    )
                    .await?;
                    Ok(Outcome::Rechecking)
                }
                Step::File => {
                    if !reserve_slot(connection, &fingerprint).await? {
                        mark(connection, &fingerprint, State::Withheld, WITHHELD).await?;
                        return Ok(Outcome::Withheld);
                    }

                    // Read before the grant below sets it again, so it
                    // describes the attempt before this one.
                    let retrying = row.create_unconfirmed;
                    grant(connection, &fingerprint).await?;
                    jobs::enqueue_query(
                        connection,
                        &FileErrorReport {
                            fingerprint: fingerprint.clone(),
                            action: Action::File { retrying },
                        },
                    )
                    .await?;
                    Ok(Outcome::Queued)
                }
            }
        })
        .await
}

/// Whether anything has ever been recorded under `fingerprint`.
///
/// For an application bounding what an anonymous caller may add: a fault that
/// is already known costs a count, and one that is not costs a row.
///
/// # Errors
/// Returns the database's error when the lookup fails.
pub async fn seen(connection: &mut AsyncPgConnection, fingerprint: &str) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        error_reports::table.filter(error_reports::fingerprint.eq(fingerprint)),
    ))
    .get_result(connection)
    .await
}

/// How many distinct faults `source` first produced after `since`.
///
/// # Errors
/// Returns the database's error when the count fails.
pub async fn new_since(
    connection: &mut AsyncPgConnection,
    source: Source,
    since: DateTime<Utc>,
) -> QueryResult<i64> {
    error_reports::table
        .filter(error_reports::source.eq(source.as_str()))
        .filter(error_reports::first_seen_at.gt(since))
        .count()
        .get_result(connection)
        .await
}

/// The `limit` faults seen most recently, newest first.
///
/// # Errors
/// Returns the database's error when the read fails.
pub async fn recent(
    connection: &mut AsyncPgConnection,
    limit: i64,
) -> QueryResult<Vec<ErrorReport>> {
    error_reports::table
        .order(error_reports::last_seen_at.desc())
        .limit(limit)
        .select(ErrorReport::as_select())
        .load(connection)
        .await
}

/// The row as the claim needs it.
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = error_reports, check_for_backend(diesel::pg::Pg))]
struct ClaimRow {
    state: String,
    issue_number: Option<i64>,
    occurrences: i64,
    checked_at: DateTime<Utc>,
    create_unconfirmed: bool,
}

/// The row as a filing job needs it.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = error_reports, check_for_backend(diesel::pg::Pg))]
pub(super) struct FilingRow {
    pub source: String,
    pub kind: String,
    pub title: String,
    pub sample: JsonValue,
    pub state: String,
    pub issue_number: Option<i64>,
    pub occurrences: i64,
    pub reported_occurrences: i64,
    pub last_seen_at: DateTime<Utc>,
}

impl FilingRow {
    /// Where the row stands.
    pub fn standing(&self) -> State {
        State::parse(&self.state)
    }

    /// The source it was recorded under.
    pub fn source(&self) -> Source {
        match self.source.as_str() {
            "browser" => Source::Browser,
            "worker" => Source::Worker,
            _ => Source::Server,
        }
    }

    /// The latest occurrence. A sample that no longer decodes renders as its
    /// title alone rather than failing the filing: the issue is still worth
    /// having.
    pub fn sample(&self) -> Sample {
        serde_json::from_value(self.sample.clone()).unwrap_or_else(|_unreadable| Sample {
            summary: self.title.clone(),
            ..Sample::default()
        })
    }
}

/// Reads the row a filing job is about.
pub(super) async fn load(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
) -> QueryResult<Option<FilingRow>> {
    error_reports::table
        .find(fingerprint)
        .select(FilingRow::as_select())
        .first(connection)
        .await
        .optional()
}

/// Records the issue a filing produced.
pub(super) async fn record_filed(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
    issue_number: i64,
    issue_url: &str,
    occurrences: i64,
) -> QueryResult<()> {
    diesel::update(error_reports::table.find(fingerprint))
        .set((
            error_reports::state.eq(State::Open.as_str()),
            error_reports::issue_number.eq(issue_number),
            error_reports::issue_url.eq(issue_url),
            error_reports::failure.eq(None::<String>),
            error_reports::checked_at.eq(diesel::dsl::now),
            error_reports::filed_at.eq(diesel::dsl::now),
            error_reports::create_unconfirmed.eq(false),
            error_reports::reported_occurrences.eq(occurrences),
        ))
        .execute(connection)
        .await?;
    Ok(())
}

/// Records that the open issue has been told about `occurrences`.
pub(super) async fn record_reported(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
    occurrences: i64,
) -> QueryResult<()> {
    diesel::update(error_reports::table.find(fingerprint))
        .set(error_reports::reported_occurrences.eq(occurrences))
        .execute(connection)
        .await?;
    Ok(())
}

/// Moves a row to a state with no issue, saying why.
///
/// One writer for every way a filing can end without an issue: refused by the
/// ceiling, refused by the tracker, and nothing to file with. **The issue is
/// cleared as part of it**, because `issue_number` and `issue_url` describe a
/// row's current issue and such a row has none. `create_unconfirmed` is left
/// alone: a refusal after a create that may have happened does not undo it.
pub(super) async fn mark(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
    state: State,
    failure: &str,
) -> QueryResult<()> {
    diesel::update(error_reports::table.find(fingerprint))
        .set((
            error_reports::state.eq(state.as_str()),
            error_reports::failure.eq(failure),
            error_reports::checked_at.eq(diesel::dsl::now),
            error_reports::issue_number.eq(None::<i64>),
            error_reports::issue_url.eq(None::<String>),
        ))
        .execute(connection)
        .await?;
    Ok(())
}

/// Hands a row back to filing after its issue was found closed.
///
/// **Takes a slot like any other filing.** A refile produces an issue, so it
/// spends budget, and bulk-closing stale issues before a deploy that regresses
/// them would otherwise give every recheck its own unbudgeted filing.
///
/// Returns whether it got one; a row that did not is left `withheld`.
pub(super) async fn refile(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
) -> QueryResult<bool> {
    connection
        .transaction::<bool, diesel::result::Error, _>(async |connection| {
            if reserve_slot(connection, fingerprint).await? {
                grant(connection, fingerprint).await?;
                return Ok(true);
            }

            mark(connection, fingerprint, State::Withheld, WITHHELD).await?;
            Ok(false)
        })
        .await
}

/// Marks a row as filing, before the request goes out.
///
/// The issue is cleared with the state: a row that is filing does not have one
/// yet, and a refile arrives carrying the number of the issue somebody closed.
/// `create_unconfirmed` goes true here, which is what lets the next attempt
/// know this one may have created an issue it never recorded.
async fn grant(connection: &mut AsyncPgConnection, fingerprint: &str) -> QueryResult<()> {
    diesel::update(error_reports::table.find(fingerprint))
        .set((
            error_reports::state.eq(State::Queued.as_str()),
            error_reports::checked_at.eq(diesel::dsl::now),
            error_reports::issue_number.eq(None::<i64>),
            error_reports::issue_url.eq(None::<String>),
            error_reports::failure.eq(None::<String>),
            error_reports::create_unconfirmed.eq(true),
        ))
        .execute(connection)
        .await?;
    Ok(())
}

/// Takes one of the window's filing slots, or answers that there are none.
///
/// **The reservation is the point.** Reading a count and then filing is a
/// check-then-act across a round trip to the tracker, so every member of a
/// simultaneous burst would read the same count and every one would proceed.
///
/// **It counts filings, not rows.** A row holds one `filed_at`, so the two
/// halves are summed rather than combined:
///
/// | Half | Counts |
/// |---|---|
/// | `filed_at` inside the window | An issue a row already created, **this row's own included** |
/// | `queued` since the window opened, excluding this row | A filing somebody else has started and not finished |
///
/// The exclusion belongs to the second half alone: a brand new fault is
/// inserted `queued` before it asks, and must not count itself. Excluding it
/// from the first half too would drop the row's own previous issue, and ten
/// closed issues refiling one after another would each see only nine.
///
/// The advisory lock serializes filing decisions against each other and
/// nothing else: an occurrence that is merely counted never reaches this.
async fn reserve_slot(connection: &mut AsyncPgConnection, fingerprint: &str) -> QueryResult<bool> {
    diesel::sql_query(format!("SELECT pg_advisory_xact_lock({CEILING_LOCK})"))
        .execute(connection)
        .await?;

    let window_opened = Utc::now() - CEILING_WINDOW;
    let filed: i64 = error_reports::table
        .filter(error_reports::filed_at.gt(window_opened))
        .count()
        .get_result(connection)
        .await?;
    let filing: i64 = error_reports::table
        .filter(error_reports::fingerprint.ne(fingerprint))
        .filter(error_reports::state.eq(State::Queued.as_str()))
        .filter(error_reports::checked_at.gt(window_opened))
        .count()
        .get_result(connection)
        .await?;

    Ok(filed + filing < MAX_ISSUES_PER_WINDOW)
}

/// What an occurrence of an already-recorded fault should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Count,
    Recheck,
    File,
}

/// The deduplication rule, over a row's state and how stale it is.
///
/// `since_last_call` is the time since anything was asked of the tracker about
/// this fingerprint, which bounds both the recheck and the retry.
fn next_step(state: State, since_last_call: TimeDelta) -> Step {
    match state {
        // An open issue already says this. Confirm it is still open, and tell
        // it how often, no more than once an hour per fault.
        State::Open if since_last_call >= RECHECK_AFTER => Step::Recheck,
        // A filing that failed, was withheld, had nothing to file with, or
        // whose process died holding the claim. All retry, and all wait.
        State::Queued | State::Failed | State::Withheld | State::Unconfigured
            if since_last_call >= RETRY_AFTER =>
        {
            Step::File
        }
        // Covered already: an open issue looked at recently, a filing in
        // flight, or a failure still inside its backoff.
        State::Open | State::Queued | State::Failed | State::Withheld | State::Unconfigured => {
            Step::Count
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::{RECHECK_AFTER, RETRY_AFTER, State, Step, next_step};

    #[test]
    fn an_open_issue_absorbs_a_recurrence() {
        // The whole point: a fault in a loop is one issue, not thousands.
        assert_eq!(next_step(State::Open, TimeDelta::seconds(1)), Step::Count);
        assert_eq!(
            next_step(State::Open, RECHECK_AFTER - TimeDelta::seconds(1)),
            Step::Count
        );
    }

    #[test]
    fn an_open_issue_is_rechecked_once_the_window_passes() {
        assert_eq!(next_step(State::Open, RECHECK_AFTER), Step::Recheck);
        assert_eq!(next_step(State::Open, TimeDelta::days(3)), Step::Recheck);
    }

    #[test]
    fn every_unfiled_state_waits_before_retrying() {
        // Otherwise an outage during a hot loop is a call per occurrence.
        for state in [State::Failed, State::Withheld, State::Unconfigured] {
            assert_eq!(
                next_step(state, TimeDelta::minutes(1)),
                Step::Count,
                "{state:?}"
            );
            assert_eq!(next_step(state, RETRY_AFTER), Step::File, "{state:?}");
        }
    }

    #[test]
    fn an_abandoned_claim_is_retried_rather_than_stuck() {
        assert_eq!(
            next_step(State::Queued, TimeDelta::seconds(5)),
            Step::Count,
            "a filing in flight must not be filed twice"
        );
        assert_eq!(next_step(State::Queued, TimeDelta::hours(2)), Step::File);
    }

    #[test]
    fn an_unknown_state_reads_as_failed() {
        assert_eq!(State::parse("reopened"), State::Failed);
        for state in [
            State::Queued,
            State::Open,
            State::Failed,
            State::Withheld,
            State::Unconfigured,
        ] {
            assert_eq!(State::parse(state.as_str()), state);
        }
    }
}
