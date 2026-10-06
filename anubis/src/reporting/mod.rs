//! Turning the application's own failures into de-duplicated tracker issues.
//!
//! Anything that means "the system failed, crashed, or did not behave as it
//! should" becomes a [`Report`]: a `500`, a panic, an `ERROR` log line, a
//! browser that crashed rendering a page, a background pass that fell over, a
//! credential a vendor refused. Every report is counted against a fingerprint
//! in the `error_reports` table, and a fingerprint becomes **one** issue in the
//! tracker the application configured, however often it recurs.
//!
//! # The pieces
//!
//! | Piece | What it does |
//! |---|---|
//! | [`Report`] | One occurrence: a source, a kind, a summary, facts, a detail. Redacted and clipped as it is built. |
//! | [`record`] | Counts an occurrence and, in the same transaction, queues the filing job when one is due. |
//! | [`Reporter`] | A cheap, non-blocking handle for code that cannot await: a panic hook, a tracing layer. |
//! | [`Inbox`] | The task that drains a [`Reporter`] into [`record`]. |
//! | [`Filer`] | Runs [`FileErrorReport`] jobs against a [`Tracker`]. Register it on a [`crate::jobs::Worker`]. |
//! | [`Tracker`] | The seam: where issues go. [`github::GitHub`] is one implementation. |
//! | [`ErrorLayer`], [`install_panic_hook`], [`instrument`] | Capture: `ERROR` events, panics, and `500`s nobody logged. |
//!
//! # Wiring an application
//!
//! ```ignore
//! let (reporter, inbox) = anubis::reporting::Reporter::new(Source::Server);
//! anubis::telemetry::init_with_reporting(&config, &reporter)?;
//! anubis::reporting::install_panic_hook(reporter.clone());
//!
//! // ...once the pool exists:
//! tokio::spawn(inbox.run(pool.clone(), shutdown));
//! let app = anubis::reporting::instrument(app, reporter.clone());
//!
//! // ...and on the job worker, in one process only:
//! let filer = Filer::new(pool.clone(), Arc::new(GitHub::new(credentials)), "My product");
//! worker.register(move |job: FileErrorReport| {
//!     let filer = filer.clone();
//!     async move { filer.file(job).await }
//! })
//! ```
//!
//! # Never in the way
//!
//! Reporting must not make a failure worse. [`Reporter::report`] never blocks
//! and never fails: it hands the report to a bounded channel and drops it, with
//! a count, when the channel is full. Recording is one upsert. Filing is a
//! durable job that runs later, so a tracker outage costs a delay rather than a
//! request, and the tracker is never called on a request path.
//!
//! # One issue per fault
//!
//! The policy is QA Platform's, which was tested there first:
//!
//! - **The first occurrence files.** The fingerprint has no row, so the claim
//!   that inserts it queues a filing.
//! - **Recurrences are counted, not filed.** The row's lock serializes the
//!   count, so a burst cannot file twice.
//! - **An open issue is rechecked at most hourly.** The recheck asks the
//!   tracker whether it is still open; when it is, and there were occurrences
//!   since it last heard, it comments once with the count. When somebody closed
//!   it, the fault came back after being called fixed, and a fresh issue is
//!   filed that names the closed one. Closed issues are never reopened.
//! - **Failures retry on a backoff**, measured from the last call, so an outage
//!   during a hot loop is not a call per occurrence.
//! - **At most [`MAX_ISSUES_PER_WINDOW`] issues an hour**, reserved under an
//!   advisory lock so a burst of distinct faults cannot all pass a count they
//!   all read at once. A withheld fault is still counted.
//!
//! # Not reporting itself
//!
//! Everything in this module logs under the `anubis::reporting` target, and
//! [`ErrorLayer`] ignores that target and the job worker's lines about
//! [`FileErrorReport`]. Without that, a tracker outage would file issues about
//! failing to file issues.

mod capture;
mod filing;
mod fingerprint;
pub mod github;
mod issue;
mod redact;
mod store;

use std::collections::HashSet;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::db::DbPool;

#[doc(inline)]
pub use capture::{ErrorLayer, install_panic_hook, instrument};
#[doc(inline)]
pub use filing::{
    FileErrorReport, FiledIssue, Filer, NewIssue, Tracker, TrackerError, TrackerFuture,
};
#[doc(inline)]
pub use fingerprint::{FINGERPRINT_CHARS, fingerprint, normalize, significant_frames};
#[doc(inline)]
pub use issue::{clip, title};
#[doc(inline)]
pub use redact::redact;
#[doc(inline)]
pub use store::{
    ErrorReport, MAX_ISSUES_PER_WINDOW, Outcome, State, new_since, recent, record, seen,
};

/// Longest summary a report keeps, in characters.
///
/// A summary is one line read in a list. Five hundred is a long sentence and a
/// short stack line, and keeps a stranger's message from becoming a page.
pub const SUMMARY_LIMIT: usize = 500;

/// Longest signature a report keeps, in characters.
///
/// Normalized and hashed rather than read, so it only has to be long enough to
/// tell two faults apart: a message and a few frames.
pub const SIGNATURE_LIMIT: usize = 2_000;

/// Longest fact value a report keeps, in characters.
///
/// A path, a user agent, a request id. Three hundred covers every honest one.
pub const FACT_LIMIT: usize = 300;

/// Most facts one report carries.
///
/// A table in an issue, read by a person. Twenty rows is already a lot.
pub const MAX_FACTS: usize = 20;

/// Longest detail a report keeps, in characters.
///
/// A stack or a backtrace. The issue body quotes the same amount, so storing
/// more would keep text nobody can see.
pub const DETAIL_LIMIT: usize = 8_000;

/// How many reports a [`Reporter`] holds before it starts dropping them.
///
/// The channel between a panic hook or a log line and the database. A thousand
/// is minutes of a pathological loop, and the loop's later occurrences would
/// only have counted against the same fingerprint anyway.
const INBOX_CAPACITY: usize = 1_024;

/// Most request ids [`Reporter`] remembers as having logged an error.
///
/// The memory [`instrument`] reads to tell a `500` that was explained from one
/// that was not. Entries leave when their response does; this bound only
/// matters if responses stopped leaving, and then forgetting is the safe
/// failure, because it costs one extra report rather than unbounded memory.
const MAX_FLAGGED_REQUESTS: usize = 4_096;

/// Which part of the application noticed.
///
/// An enum rather than a string because it is half of every fingerprint, and a
/// typo would silently split one fault into two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The process serving requests, including its background jobs.
    Server,
    /// A browser running the frontend, reported to the server.
    Browser,
    /// A process doing background work of its own, such as a rating worker.
    Worker,
}

impl Source {
    /// The name stored on the row and shown in an issue title.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Browser => "browser",
            Self::Worker => "worker",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One labelled value in a report, rendered as a table row in its issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// What the value is, written by the application rather than the failure.
    pub label: String,
    /// The value, redacted and clipped.
    pub value: String,
}

/// What an issue is rendered from: the latest occurrence, already redacted.
///
/// Stored as the row's `sample`, so the filing job reads it back rather than
/// carrying it in its own payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// One line: what happened.
    pub summary: String,
    /// Labelled values, in the order they were given.
    pub facts: Vec<Fact>,
    /// A stack, a backtrace, or anything else worth quoting verbatim.
    pub detail: String,
}

/// One occurrence of something that went wrong, on its way to an issue.
///
/// **Redacted and clipped as it is built.** Every text a report holds passes
/// through [`redact`] and a length limit the moment it arrives, so there is no
/// path by which unmasked text reaches the database or the tracker, and no
/// caller has to remember to ask.
///
/// # Examples
/// ```
/// use anubis::reporting::{Report, Source};
///
/// let report = Report::new(Source::Server, "http.request.failed", "GET /x answered 500")
///     .signature("GET /x")
///     .fact("Request id", "6f0c...")
///     .detail("connection refused");
///
/// assert_eq!(report.summary(), "GET /x answered 500");
/// assert_eq!(report.fingerprint().len(), anubis::reporting::FINGERPRINT_CHARS);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    source: Source,
    kind: &'static str,
    summary: String,
    signature: String,
    subject: String,
    facts: Vec<Fact>,
    detail: String,
}

impl Report {
    /// Starts a report: where it came from, what kind of fault, and one line.
    ///
    /// `kind` is a dotted identifier from a fixed vocabulary, such as
    /// `http.request.failed`, and is `&'static str` on purpose: letting it be
    /// built from runtime data would put unbounded cardinality into every
    /// fingerprint. The signature defaults to the summary.
    #[must_use]
    pub fn new(source: Source, kind: &'static str, summary: impl AsRef<str>) -> Self {
        let summary = clip(&redact(summary.as_ref()), SUMMARY_LIMIT);
        Self {
            source,
            kind,
            signature: clip(&summary, SIGNATURE_LIMIT),
            summary,
            subject: String::new(),
            facts: Vec::new(),
            detail: String::new(),
        }
    }

    /// Sets what two occurrences of this fault have in common.
    ///
    /// Separate from the summary because a summary is written to be read and a
    /// signature is written to be hashed. A summary naming a request id is
    /// useful; a fingerprint over one is a new issue every time. It is
    /// normalized before hashing, so ids and numbers in it do no harm.
    #[must_use]
    pub fn signature(mut self, signature: impl AsRef<str>) -> Self {
        self.signature = clip(&redact(signature.as_ref()), SIGNATURE_LIMIT);
        self
    }

    /// Names the one thing this fault is about, splitting it per thing.
    ///
    /// For a fault that is a different problem on each of several things, such
    /// as a credential a vendor refused: two refused credentials are two
    /// rotations, so they are two issues. Hashed as written rather than
    /// normalized, because normalization would mask the id that tells them
    /// apart. Most faults have no subject.
    #[must_use]
    pub fn about(mut self, subject: impl AsRef<str>) -> Self {
        self.subject = clip(&redact(subject.as_ref()), FACT_LIMIT);
        self
    }

    /// Adds a labelled value, rendered as a row in the issue's table.
    ///
    /// Past [`MAX_FACTS`] the fact is dropped: the table is read by a person.
    #[must_use]
    pub fn fact(mut self, label: impl Into<String>, value: impl AsRef<str>) -> Self {
        if self.facts.len() < MAX_FACTS {
            self.facts.push(Fact {
                label: label.into(),
                value: clip(&redact(value.as_ref()), FACT_LIMIT),
            });
        }
        self
    }

    /// Sets the text quoted verbatim in the issue, such as a stack.
    #[must_use]
    pub fn detail(mut self, detail: impl AsRef<str>) -> Self {
        self.detail = clip(&redact(detail.as_ref()), DETAIL_LIMIT);
        self
    }

    /// Which part of the application noticed.
    #[must_use]
    pub fn source(&self) -> Source {
        self.source
    }

    /// The dotted kind this report was filed under.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The one line a person reads, redacted.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// The labelled values, redacted.
    #[must_use]
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// The verbatim detail, redacted.
    #[must_use]
    pub fn detail_text(&self) -> &str {
        &self.detail
    }

    /// The fingerprint this report is counted against.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(self.source, self.kind, &self.subject, &self.signature)
    }

    /// What its issue is rendered from.
    fn sample(&self) -> Sample {
        Sample {
            summary: self.summary.clone(),
            facts: self.facts.clone(),
            detail: self.detail.clone(),
        }
    }
}

/// Hands reports to the database without ever making the caller wait.
///
/// For code that cannot await or must not fail: a panic hook, a tracing layer,
/// a worker deep inside a pass. [`Reporter::report`] pushes onto a bounded
/// channel and returns; an [`Inbox`] drains it into [`record`]. Cheap to clone,
/// and every clone feeds the same inbox.
#[derive(Clone)]
pub struct Reporter {
    inner: Arc<Inner>,
}

struct Inner {
    source: Source,
    /// `None` for a reporter that records nothing, such as in a test.
    sender: Option<mpsc::Sender<Report>>,
    /// Reports dropped because the inbox was full, announced by the inbox.
    dropped: AtomicU64,
    /// Requests that logged an error, so a `500` they answer is not reported
    /// a second time. See [`instrument`].
    flagged: Mutex<HashSet<String>>,
}

impl Reporter {
    /// A reporter for `source`, and the inbox that drains it.
    ///
    /// Spawn [`Inbox::run`] once the database pool exists. Reports made before
    /// then wait in the channel, which is what lets the reporter be created
    /// before telemetry is installed and the pool after it.
    #[must_use]
    pub fn new(source: Source) -> (Self, Inbox) {
        let (sender, receiver) = mpsc::channel(INBOX_CAPACITY);
        let reporter = Self {
            inner: Arc::new(Inner {
                source,
                sender: Some(sender),
                dropped: AtomicU64::new(0),
                flagged: Mutex::new(HashSet::new()),
            }),
        };

        (reporter.clone(), Inbox { receiver, reporter })
    }

    /// A reporter that records nothing, for tests and one-off tools.
    #[must_use]
    pub fn disabled(source: Source) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                sender: None,
                dropped: AtomicU64::new(0),
                flagged: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// The source this process reports as.
    #[must_use]
    pub fn source(&self) -> Source {
        self.inner.source
    }

    /// Queues `report` to be recorded, and returns at once.
    ///
    /// Never blocks and never fails. A full inbox drops the report and counts
    /// it; the inbox says how many it lost once it catches up. It does not log
    /// here, because this is called from inside a tracing layer and a panic
    /// hook, and a log line from either is a report about a report.
    pub fn report(&self, report: Report) {
        let Some(sender) = &self.inner.sender else {
            return;
        };

        if sender.try_send(report).is_err() {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Remembers that `request_id` logged an error.
    fn flag_request(&self, request_id: &str) {
        let mut flagged = self
            .inner
            .flagged
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if flagged.len() >= MAX_FLAGGED_REQUESTS {
            flagged.clear();
        }
        flagged.insert(request_id.to_owned());
    }

    /// Forgets `request_id`, answering whether it had logged an error.
    fn take_flag(&self, request_id: &str) -> bool {
        self.inner
            .flagged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(request_id)
    }
}

impl Debug for Reporter {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reporter")
            .field("source", &self.inner.source)
            .field("enabled", &self.inner.sender.is_some())
            .finish_non_exhaustive()
    }
}

/// Drains a [`Reporter`] into the database.
#[derive(Debug)]
pub struct Inbox {
    receiver: mpsc::Receiver<Report>,
    /// For the dropped count, which lives on the reporter it describes.
    reporter: Reporter,
}

impl Inbox {
    /// Records reports until `shutdown` resolves, then records what is queued.
    ///
    /// A report that cannot be recorded is logged and let go: the fault it
    /// describes already happened and was handled, and losing its count is
    /// better than retrying into a database that is the reason it failed.
    pub async fn run(mut self, pool: DbPool, shutdown: impl Future<Output = ()> + Send) {
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                received = self.receiver.recv() => match received {
                    Some(report) => self.store(&pool, &report).await,
                    None => return,
                },
            }
        }

        // A deploy should not lose the panic that may be why it is happening.
        while let Ok(report) = self.receiver.try_recv() {
            self.store(&pool, &report).await;
        }
    }

    async fn store(&self, pool: &DbPool, report: &Report) {
        let dropped = self.reporter.inner.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            tracing::warn!(
                reporting.dropped = dropped,
                "the error report inbox was full, so {{reporting.dropped}} reports were dropped",
            );
        }

        let mut connection = match pool.get().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(
                    error.message = %error,
                    report.kind = report.kind(),
                    "an error report could not reach the database: {{error.message}}",
                );
                return;
            }
        };

        if let Err(error) = record(&mut connection, report).await {
            tracing::warn!(
                error.message = %error,
                report.kind = report.kind(),
                "an error report could not be recorded: {{error.message}}",
            );
        }
    }
}
