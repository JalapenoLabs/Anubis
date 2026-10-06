//! The job that turns a counted fault into an issue, and the seam it files to.
//!
//! [`super::record`] decides, under the row's lock, that a fingerprint needs
//! an issue or a recheck, and enqueues a [`FileErrorReport`] in the same
//! transaction. [`Filer::file`] runs that job later, on whatever worker the
//! application registered it on, and is the only place the [`Tracker`] is
//! called. Nothing here runs on a request path.
//!
//! # At least once, without filing twice
//!
//! The job queue delivers at least once, and filing has a side effect in
//! another system. So the row carries `create_unconfirmed`, set when the filing
//! was granted, before the request went out; a job told it may be retrying
//! asks the tracker for an open issue carrying the fingerprint before it
//! creates one, and adopts what it finds. That search is the one call here that
//! reads an index which lags, and it is safe because what it looks for is at
//! least [`RETRY_AFTER`](super::store) old.
//!
//! # What retries and what does not
//!
//! | The tracker said | The job | The row |
//! |---|---|---|
//! | Not configured | succeeds | `unconfigured`, retried by a later occurrence |
//! | Refused (bad token, no access, gone) | succeeds | `failed`, with the tracker's words |
//! | Unavailable (outage, rate limit) | fails, and the queue retries it | `queued` |
//!
//! A refusal is not retried by the queue, because a revoked token refuses all
//! five attempts and each one is a call that changes nothing. An outage is.

use std::fmt::{self, Debug, Display, Formatter};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::issue::{self, Opening, recurrence_comment};
use super::store::{self, FilingRow, State};
use crate::db::DbPool;
use crate::jobs::{BoxError, Job};

/// The queue filing runs on.
///
/// Its own, so an outage at the tracker retrying its way through the backoff
/// never sits in front of the application's other work.
pub const QUEUE: &str = "error_reports";

/// What a filing job was queued to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Open an issue for the fingerprint.
    File {
        /// Whether an earlier attempt may already have created one. See the
        /// module docs.
        retrying: bool,
    },
    /// Ask whether the open issue is still open, tell it how often the fault
    /// recurred, and file again when somebody closed it.
    Recheck,
}

/// Files, or rechecks, the issue for one fingerprint.
///
/// Queued by [`super::record`]; register [`Filer::file`] as its handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileErrorReport {
    /// The row this is about. The issue is rendered from what the row holds,
    /// so the payload carries nothing a later occurrence could have improved.
    pub fingerprint: String,
    pub action: Action,
}

impl Job for FileErrorReport {
    /// Also how [`super::ErrorLayer`] recognizes the job worker's lines about
    /// this job, which it must not report.
    const KIND: &'static str = "anubis_file_error_report";
    const QUEUE: &'static str = QUEUE;
    /// Three, because the queue's backoff makes that a minute of trying, which
    /// is far inside the fifteen minutes before a later occurrence may file the
    /// same fault again. A longer run of retries could still be going when that
    /// second filing starts.
    const MAX_ATTEMPTS: i32 = 3;
}

/// An issue about to be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIssue {
    pub title: String,
    /// Markdown, with the fingerprint in it. See [`Tracker::find_open`].
    pub body: String,
    pub labels: Vec<String>,
}

/// An issue the tracker holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiledIssue {
    pub number: i64,
    /// Where a person reads it.
    pub url: String,
}

/// What a [`Tracker`] call returns.
pub type TrackerFuture<'a, Value> =
    Pin<Box<dyn Future<Output = Result<Value, TrackerError>> + Send + 'a>>;

/// Where issues go: the seam an application configures.
///
/// [`super::github::GitHub`] is the implementation the framework ships. A test
/// implements this over a vector; another tracker is another implementation.
///
/// Boxed futures rather than `async fn`, because the [`Filer`] holds one behind
/// `dyn` and a trait with an `async fn` is not dyn-compatible.
pub trait Tracker: Debug + Send + Sync + 'static {
    /// Opens an issue.
    ///
    /// # Errors
    /// A [`TrackerError`] saying whether waiting can help.
    fn create<'a>(&'a self, issue: &'a NewIssue) -> TrackerFuture<'a, FiledIssue>;

    /// Finds an open issue whose body carries `fingerprint`.
    ///
    /// **Recovery, not deduplication.** Asked only by a filing that may be
    /// repeating one which never recorded its issue. It must search bodies
    /// only and open issues only: a fingerprint pasted into a comment must not
    /// make an unrelated issue adoptable, and a closed one must not be adopted
    /// over the rule that a recurrence after a fix is filed fresh.
    ///
    /// # Errors
    /// A [`TrackerError`] saying whether waiting can help.
    fn find_open<'a>(&'a self, fingerprint: &'a str) -> TrackerFuture<'a, Option<FiledIssue>>;

    /// Whether issue `number` is still open.
    ///
    /// # Errors
    /// A [`TrackerError`] saying whether waiting can help.
    fn is_open(&self, number: i64) -> TrackerFuture<'_, bool>;

    /// Adds a comment to issue `number`.
    ///
    /// # Errors
    /// A [`TrackerError`] saying whether waiting can help.
    fn comment<'a>(&'a self, number: i64, body: &'a str) -> TrackerFuture<'a, ()>;
}

/// Why a [`Tracker`] call did not do what was asked.
///
/// The three cases are the three things the [`Filer`] does differently: record
/// and stop, record the refusal and stop, or fail the job so the queue retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerError {
    kind: TrackerErrorKind,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackerErrorKind {
    NotConfigured,
    Refused,
    Unavailable,
}

impl TrackerError {
    /// Nothing is configured to file with, which is a fresh deployment's state.
    #[must_use]
    pub fn not_configured() -> Self {
        Self {
            kind: TrackerErrorKind::NotConfigured,
            message: "No issue tracker is configured, so this fault is counted and not filed."
                .to_owned(),
        }
    }

    /// The tracker refused, and asking again will not change its mind.
    ///
    /// `message` is shown to an operator and stored on the row, so it must
    /// never quote the credential.
    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            kind: TrackerErrorKind::Refused,
            message: message.into(),
        }
    }

    /// The tracker could not answer right now; waiting may help.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: TrackerErrorKind::Unavailable,
            message: message.into(),
        }
    }

    /// Whether nothing is configured.
    #[must_use]
    pub fn is_not_configured(&self) -> bool {
        self.kind == TrackerErrorKind::NotConfigured
    }

    /// Whether the tracker refused.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        self.kind == TrackerErrorKind::Refused
    }

    /// Whether waiting may help.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        self.kind == TrackerErrorKind::Unavailable
    }

    /// What went wrong, in words an operator can act on.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for TrackerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TrackerError {}

/// Labels put on every filed issue.
///
/// GitHub creates a missing label rather than refusing the issue, so a typo
/// would quietly invent one nobody filters on. `bug` exists on every new
/// repository.
const LABELS: [&str; 1] = ["bug"];

/// Runs [`FileErrorReport`] jobs against a [`Tracker`]. Cheap to clone.
#[derive(Clone)]
pub struct Filer {
    pool: DbPool,
    tracker: Arc<dyn Tracker>,
    /// Named in every issue's footer, so a reader knows what filed it.
    product: Arc<str>,
}

impl Debug for Filer {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filer")
            .field("tracker", &self.tracker)
            .field("product", &self.product)
            .finish_non_exhaustive()
    }
}

impl Filer {
    /// A filer that files to `tracker`, signing issues as `product`.
    #[must_use]
    pub fn new(pool: DbPool, tracker: Arc<dyn Tracker>, product: &str) -> Self {
        Self {
            pool,
            tracker,
            product: Arc::from(product),
        }
    }

    /// Runs one job. Register this as the job's handler.
    ///
    /// # Errors
    /// Fails, so the queue retries, when the tracker is unavailable or the
    /// database is. Everything else, including a refusal, is recorded on the
    /// row and succeeds.
    pub async fn file(&self, job: FileErrorReport) -> Result<(), BoxError> {
        let mut connection = self.pool.get().await?;
        let Some(row) = store::load(&mut connection, &job.fingerprint).await? else {
            // Nothing deletes a row today, so this is a fingerprint somebody
            // removed by hand. There is nothing left to file.
            tracing::warn!(
                report.fingerprint = %job.fingerprint,
                "a filing job named a fingerprint with no row, so nothing was filed",
            );
            return Ok(());
        };
        drop(connection);

        match job.action {
            Action::File { retrying } => {
                // A row that is no longer queued was filed, or refused, by an
                // earlier delivery of this same job.
                if row.standing() != State::Queued {
                    return Ok(());
                }
                self.open(&job.fingerprint, &row, retrying, None).await
            }
            Action::Recheck => self.recheck(&job.fingerprint, &row).await,
        }
    }

    /// Opens the issue, adopting one an earlier attempt filed if it may have.
    async fn open(
        &self,
        fingerprint: &str,
        row: &FilingRow,
        retrying: bool,
        recurrence_of: Option<i64>,
    ) -> Result<(), BoxError> {
        let adopted = if retrying {
            match self.tracker.find_open(fingerprint).await {
                Ok(found) => found,
                Err(error) => return self.unfiled(fingerprint, error).await,
            }
        } else {
            None
        };

        let filed = if let Some(found) = adopted {
            tracing::warn!(
                issue.number = found.number,
                report.fingerprint = %fingerprint,
                "adopted issue {{issue.number}}, which an earlier attempt filed without recording",
            );
            found
        } else {
            let sample = row.sample();
            let new_issue = NewIssue {
                title: row.title.clone(),
                body: issue::body(&Opening {
                    source: row.source(),
                    kind: &row.kind,
                    fingerprint,
                    occurrences: row.occurrences,
                    sample: &sample,
                    product: &self.product,
                    recurrence_of,
                }),
                labels: LABELS.iter().map(|label| (*label).to_owned()).collect(),
            };

            match self.tracker.create(&new_issue).await {
                Ok(created) => created,
                Err(error) => return self.unfiled(fingerprint, error).await,
            }
        };

        let mut connection = self.pool.get().await?;
        store::record_filed(
            &mut connection,
            fingerprint,
            filed.number,
            &filed.url,
            row.occurrences,
        )
        .await?;

        tracing::info!(
            issue.number = filed.number,
            issue.url = %filed.url,
            report.fingerprint = %fingerprint,
            "filed issue {{issue.number}} for fault {{report.fingerprint}}",
        );
        Ok(())
    }

    /// Asks whether the open issue is still open, and acts on the answer.
    async fn recheck(&self, fingerprint: &str, row: &FilingRow) -> Result<(), BoxError> {
        let (State::Open, Some(number)) = (row.standing(), row.issue_number) else {
            // Refiled or refused since the recheck was queued.
            return Ok(());
        };

        match self.tracker.is_open(number).await {
            Ok(true) => {
                self.tell(fingerprint, row, number).await;
                Ok(())
            }
            // Closed, so the fault came back after somebody called it fixed. A
            // replacement spends budget like any other filing.
            Ok(false) => {
                let mut connection = self.pool.get().await?;
                if !store::refile(&mut connection, fingerprint).await? {
                    tracing::warn!(
                        issue.number = number,
                        "the hourly issue ceiling is taken, so the recurrence of a closed \
                         issue goes unfiled for now",
                    );
                    return Ok(());
                }
                drop(connection);

                // Never a retry: this is the first attempt at the replacement,
                // and a search now would find the issue just closed while the
                // index still calls it open.
                self.open(fingerprint, row, false, Some(number)).await
            }
            // Erring toward silence: an unreadable issue is assumed open, so an
            // outage costs a comment rather than a duplicate.
            Err(error) => {
                tracing::warn!(
                    issue.number = number,
                    error.message = %error,
                    "could not read whether issue {{issue.number}} is open, so it is assumed \
                     to be: {{error.message}}",
                );
                Ok(())
            }
        }
    }

    /// Comments on an open issue with the occurrences it has not heard about.
    ///
    /// Best effort. A comment that fails is not retried by the queue: the
    /// count is not marked as told, so the next hourly recheck says it.
    async fn tell(&self, fingerprint: &str, row: &FilingRow, number: i64) {
        let since_reported = row.occurrences - row.reported_occurrences;
        if since_reported <= 0 {
            return;
        }

        let comment = recurrence_comment(
            since_reported,
            row.occurrences,
            row.last_seen_at,
            &row.sample(),
        );
        if let Err(error) = self.tracker.comment(number, &comment).await {
            tracing::warn!(
                issue.number = number,
                error.message = %error,
                "could not tell issue {{issue.number}} how often it recurred: {{error.message}}",
            );
            return;
        }

        let recorded = match self.pool.get().await {
            Ok(mut connection) => {
                store::record_reported(&mut connection, fingerprint, row.occurrences).await
            }
            Err(error) => {
                tracing::warn!(
                    error.message = %error,
                    "the recurrence count could not be recorded: {{error.message}}",
                );
                return;
            }
        };
        if let Err(error) = recorded {
            tracing::warn!(
                error.message = %error,
                "the recurrence count could not be recorded: {{error.message}}",
            );
        }
    }

    /// Records a filing that produced no issue, or fails the job to retry it.
    async fn unfiled(&self, fingerprint: &str, error: TrackerError) -> Result<(), BoxError> {
        if error.is_unavailable() {
            return Err(Box::new(error));
        }

        let state = if error.is_not_configured() {
            State::Unconfigured
        } else {
            State::Failed
        };

        let mut connection = self.pool.get().await?;
        store::mark(&mut connection, fingerprint, state, error.message()).await?;

        if error.is_refused() {
            tracing::warn!(
                report.fingerprint = %fingerprint,
                error.message = %error,
                "the issue tracker refused fault {{report.fingerprint}}: {{error.message}}",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, FileErrorReport, TrackerError};

    #[test]
    fn the_payload_round_trips_and_names_its_action() {
        let job = FileErrorReport {
            fingerprint: "0123456789abcdef".to_owned(),
            action: Action::File { retrying: true },
        };

        let payload = serde_json::to_value(&job).expect("the payload must serialize");
        assert_eq!(payload["action"]["type"], "file");
        assert_eq!(payload["action"]["retrying"], true);

        let decoded: FileErrorReport =
            serde_json::from_value(payload).expect("the payload must decode");
        assert_eq!(decoded, job);
    }

    #[test]
    fn each_kind_of_tracker_error_answers_only_its_own_question() {
        let quiet = TrackerError::not_configured();
        let refused = TrackerError::refused("401");
        let busy = TrackerError::unavailable("503");

        assert!(quiet.is_not_configured() && !quiet.is_refused() && !quiet.is_unavailable());
        assert!(refused.is_refused() && !refused.is_unavailable());
        assert!(busy.is_unavailable() && !busy.is_refused());
        assert_eq!(refused.to_string(), "401");
    }
}
