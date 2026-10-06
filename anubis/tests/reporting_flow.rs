//! Error reporting end to end: counting, filing, rechecking, and the ceiling.
//!
//! Each narrative takes a database of its own, because they count rows and
//! the hourly ceiling is a count across the whole table. The tracker is a fake
//! that records what it was asked, except in the narratives about GitHub
//! itself, which run the real client against a fake GitHub on loopback.
//!
//! Requires `DATABASE_URL`; without it every narrative logs a skip and passes.

mod support;

use std::sync::{Arc, Mutex, MutexGuard};

use anubis::db::DbPool;
use anubis::jobs::Job;
use anubis::reporting::github::{Credentials, CredentialsFuture, GitHub, Target, Usage};
use anubis::reporting::{
    FileErrorReport, FiledIssue, Filer, MAX_ISSUES_PER_WINDOW, NewIssue, Outcome, Report, Reporter,
    Source, Tracker, TrackerError, TrackerFuture, record,
};
use anubis::schema::{error_reports, jobs};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde_json::Value as JsonValue;
use support::TestDatabase;
use tower::ServiceExt;

/// What the fake tracker answers `create` with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Accept,
    NotConfigured,
    Refuse,
    Unavailable,
}

/// A tracker that remembers what it was asked and answers as told.
#[derive(Debug)]
struct FakeTracker {
    state: Mutex<FakeState>,
}

#[derive(Debug)]
struct FakeState {
    answer: Answer,
    created: Vec<NewIssue>,
    comments: Vec<(i64, String)>,
    /// What `find_open` reports, as an issue a dead attempt filed.
    orphan: Option<FiledIssue>,
    /// Whether `is_open` says the issue is still open.
    open: bool,
}

impl FakeTracker {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                answer,
                created: Vec::new(),
                comments: Vec::new(),
                orphan: None,
                open: true,
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .expect("the fake's lock must not be poisoned")
    }

    fn failure(answer: Answer) -> Option<TrackerError> {
        match answer {
            Answer::Accept => None,
            Answer::NotConfigured => Some(TrackerError::not_configured()),
            Answer::Refuse => Some(TrackerError::refused("GitHub refused the token (401).")),
            Answer::Unavailable => {
                Some(TrackerError::unavailable("GitHub is having trouble (502)."))
            }
        }
    }
}

impl Tracker for FakeTracker {
    fn create<'a>(&'a self, issue: &'a NewIssue) -> TrackerFuture<'a, FiledIssue> {
        Box::pin(async move {
            let mut state = self.state();
            if let Some(error) = Self::failure(state.answer) {
                return Err(error);
            }
            state.created.push(issue.clone());
            let number = i64::try_from(state.created.len()).expect("a small count") + 100;
            Ok(FiledIssue {
                number,
                url: format!("https://github.com/o/r/issues/{number}"),
            })
        })
    }

    fn find_open<'a>(&'a self, _fingerprint: &'a str) -> TrackerFuture<'a, Option<FiledIssue>> {
        Box::pin(async move { Ok(self.state().orphan.clone()) })
    }

    fn is_open(&self, _number: i64) -> TrackerFuture<'_, bool> {
        Box::pin(async move { Ok(self.state().open) })
    }

    fn comment<'a>(&'a self, number: i64, body: &'a str) -> TrackerFuture<'a, ()> {
        Box::pin(async move {
            self.state().comments.push((number, body.to_owned()));
            Ok(())
        })
    }
}

/// One fault, as the same broken query reports it against a different row.
fn fault(row: usize) -> Report {
    Report::new(
        Source::Server,
        "http.request.failed",
        format!("GET /operator/entries/{row} failed: no column `titel`"),
    )
    .signature(format!("GET /operator/entries/{row}\nno column `titel`"))
    .fact("Request id", format!("request-{row}"))
}

/// Every filing job waiting in the queue, decoded, oldest first.
async fn queued_jobs(connection: &mut AsyncPgConnection) -> Vec<FileErrorReport> {
    let payloads: Vec<JsonValue> = jobs::table
        .filter(jobs::kind.eq(FileErrorReport::KIND))
        .order(jobs::created_at.asc())
        .select(jobs::payload)
        .load(connection)
        .await
        .expect("the queue must be readable");
    diesel::delete(jobs::table.filter(jobs::kind.eq(FileErrorReport::KIND)))
        .execute(connection)
        .await
        .expect("the queue must be clearable");

    payloads
        .into_iter()
        .map(|payload| serde_json::from_value(payload).expect("a filing job decodes"))
        .collect()
}

/// The row's state, issue number, and occurrence count.
async fn row(
    connection: &mut AsyncPgConnection,
    fingerprint: &str,
) -> (String, Option<i64>, i64, Option<String>) {
    error_reports::table
        .find(fingerprint)
        .select((
            error_reports::state,
            error_reports::issue_number,
            error_reports::occurrences,
            error_reports::failure,
        ))
        .first(connection)
        .await
        .expect("the row must exist")
}

/// Moves the row's last call to the tracker `minutes` into the past.
async fn age(connection: &mut AsyncPgConnection, fingerprint: &str, minutes: i64) {
    diesel::sql_query(format!(
        "UPDATE error_reports SET checked_at = now() - interval '{minutes} minutes' \
         WHERE fingerprint = '{fingerprint}'"
    ))
    .execute(connection)
    .await
    .expect("the row must be ageable");
}

async fn database(narrative: &str) -> Option<(TestDatabase, DbPool)> {
    let database = TestDatabase::create(narrative).await?;
    let pool = database.pool().await;
    Some((database, pool))
}

#[tokio::test]
async fn a_fault_in_a_loop_files_one_issue_and_counts_the_rest() {
    let Some((_database, pool)) = database("reporting_flow loop").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    let mut outcomes = Vec::new();
    for occurrence in 0..50 {
        outcomes.push(
            record(&mut connection, &fault(occurrence))
                .await
                .expect("an occurrence must record"),
        );
    }

    assert_eq!(outcomes[0], Outcome::Queued, "the first occurrence files");
    assert!(
        outcomes[1..]
            .iter()
            .all(|outcome| *outcome == Outcome::Counted),
        "every recurrence is counted, not filed: {outcomes:?}",
    );

    let filings = queued_jobs(&mut connection).await;
    assert_eq!(filings.len(), 1, "one fault is one filing job");

    let tracker = FakeTracker::new(Answer::Accept);
    let filer = Filer::new(
        pool.clone(),
        Arc::<FakeTracker>::clone(&tracker),
        "the suite",
    );
    filer
        .file(filings[0].clone())
        .await
        .expect("filing must succeed");
    // At least once: a second delivery of the same job files nothing more.
    filer
        .file(filings[0].clone())
        .await
        .expect("a repeated delivery must succeed");

    let created = tracker.state().created.clone();
    assert_eq!(created.len(), 1, "one issue, however often it ran");
    assert!(
        created[0]
            .title
            .starts_with("[server] GET /operator/entries/")
    );
    assert!(created[0].body.contains(&filings[0].fingerprint));
    assert_eq!(created[0].labels, vec!["bug".to_owned()]);

    let (state, issue, occurrences, _failure) = row(&mut connection, &filings[0].fingerprint).await;
    assert_eq!(state, "open");
    assert_eq!(issue, Some(101));
    assert_eq!(occurrences, 50);
}

#[tokio::test]
async fn nothing_is_filed_while_nothing_is_configured_and_the_count_survives() {
    let Some((_database, pool)) = database("reporting_flow unconfigured").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    record(&mut connection, &fault(1))
        .await
        .expect("an occurrence must record");
    let filings = queued_jobs(&mut connection).await;

    let tracker = FakeTracker::new(Answer::NotConfigured);
    Filer::new(
        pool.clone(),
        Arc::<FakeTracker>::clone(&tracker),
        "the suite",
    )
    .file(filings[0].clone())
    .await
    .expect("an unconfigured tracker is not a job failure");

    record(&mut connection, &fault(2))
        .await
        .expect("a recurrence must record");

    let (state, issue, occurrences, failure) = row(&mut connection, &filings[0].fingerprint).await;
    assert_eq!(state, "unconfigured");
    assert_eq!(issue, None);
    assert_eq!(occurrences, 2, "the fault is still counted");
    assert!(failure.is_some_and(|text| text.contains("No issue tracker")));
    assert!(tracker.state().created.is_empty());
}

#[tokio::test]
async fn an_outage_fails_the_job_so_the_queue_retries_it_and_a_refusal_does_not() {
    let Some((_database, pool)) = database("reporting_flow retry").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    record(&mut connection, &fault(1))
        .await
        .expect("an occurrence must record");
    let filing = queued_jobs(&mut connection).await.remove(0);

    let tracker = FakeTracker::new(Answer::Unavailable);
    let filer = Filer::new(
        pool.clone(),
        Arc::<FakeTracker>::clone(&tracker),
        "the suite",
    );
    filer
        .file(filing.clone())
        .await
        .expect_err("an outage fails the job, which is what retries it");
    assert_eq!(
        row(&mut connection, &filing.fingerprint).await.0,
        "queued",
        "a filing the queue will retry stays queued",
    );

    tracker.state().answer = Answer::Accept;
    filer
        .file(filing.clone())
        .await
        .expect("the retry must succeed once the outage passes");
    assert_eq!(row(&mut connection, &filing.fingerprint).await.0, "open");

    // A refusal on a different fault is recorded and the job succeeds, because
    // a revoked token would refuse every retry too.
    record(
        &mut connection,
        &Report::new(Source::Worker, "panic", "boom"),
    )
    .await
    .expect("an occurrence must record");
    let refused = queued_jobs(&mut connection).await.remove(0);
    tracker.state().answer = Answer::Refuse;
    filer
        .file(refused.clone())
        .await
        .expect("a refusal is not a job failure");

    let (state, issue, _occurrences, failure) = row(&mut connection, &refused.fingerprint).await;
    assert_eq!(state, "failed");
    assert_eq!(issue, None);
    assert_eq!(failure.as_deref(), Some("GitHub refused the token (401)."));
}

#[tokio::test]
async fn a_burst_of_new_faults_files_exactly_the_ceiling() {
    let Some((_database, pool)) = database("reporting_flow ceiling").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    let mut queued = 0;
    let mut withheld = 0;
    for distinct in 0..15 {
        let report = Report::new(
            Source::Browser,
            "browser.uncaught",
            format!("fault {distinct}"),
        )
        // Words rather than digits, which normalization would merge.
        .signature(format!("fault {}", "x".repeat(distinct + 1)));
        match record(&mut connection, &report)
            .await
            .expect("an occurrence must record")
        {
            Outcome::Queued => queued += 1,
            Outcome::Withheld => withheld += 1,
            other => panic!("a new fault is filed or withheld, not {other:?}"),
        }
    }

    assert_eq!(queued, MAX_ISSUES_PER_WINDOW);
    assert_eq!(withheld, 15 - MAX_ISSUES_PER_WINDOW);
}

#[tokio::test]
async fn a_retry_adopts_the_issue_an_earlier_attempt_filed() {
    let Some((_database, pool)) = database("reporting_flow adopt").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    // The first filing is granted and its process dies before it records
    // anything: the job is gone and the row still says it may have created.
    record(&mut connection, &fault(1))
        .await
        .expect("an occurrence must record");
    let abandoned = queued_jobs(&mut connection).await.remove(0);
    age(&mut connection, &abandoned.fingerprint, 20).await;

    assert_eq!(
        record(&mut connection, &fault(2))
            .await
            .expect("a recurrence must record"),
        Outcome::Queued,
        "a filing abandoned past the backoff is filed again",
    );
    let retry = queued_jobs(&mut connection).await.remove(0);
    assert_eq!(
        serde_json::to_value(retry.action).expect("an action serializes")["retrying"],
        true,
    );

    let tracker = FakeTracker::new(Answer::Accept);
    tracker.state().orphan = Some(FiledIssue {
        number: 77,
        url: "https://github.com/o/r/issues/77".to_owned(),
    });
    Filer::new(
        pool.clone(),
        Arc::<FakeTracker>::clone(&tracker),
        "the suite",
    )
    .file(retry)
    .await
    .expect("the retry must succeed");

    assert!(tracker.state().created.is_empty(), "nothing filed twice");
    assert_eq!(
        row(&mut connection, &abandoned.fingerprint).await.1,
        Some(77)
    );
}

#[tokio::test]
async fn an_open_issue_is_told_the_count_and_a_closed_one_is_filed_again() {
    let Some((_database, pool)) = database("reporting_flow recheck").await else {
        return;
    };
    let mut connection = pool.get().await.expect("a connection");

    let tracker = FakeTracker::new(Answer::Accept);
    let filer = Filer::new(
        pool.clone(),
        Arc::<FakeTracker>::clone(&tracker),
        "the suite",
    );

    record(&mut connection, &fault(1))
        .await
        .expect("an occurrence must record");
    let first = queued_jobs(&mut connection).await.remove(0);
    filer
        .file(first.clone())
        .await
        .expect("filing must succeed");

    // Four more inside the hour are counted and nothing is asked.
    for occurrence in 2..=5 {
        assert_eq!(
            record(&mut connection, &fault(occurrence))
                .await
                .expect("a recurrence must record"),
            Outcome::Counted
        );
    }

    // An hour on, the next occurrence rechecks, and the open issue hears how
    // many times it happened since it was filed.
    age(&mut connection, &first.fingerprint, 61).await;
    assert_eq!(
        record(&mut connection, &fault(6))
            .await
            .expect("a recurrence must record"),
        Outcome::Rechecking
    );
    filer
        .file(queued_jobs(&mut connection).await.remove(0))
        .await
        .expect("the recheck must succeed");

    let comments = tracker.state().comments.clone();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].0, 101);
    assert!(
        comments[0]
            .1
            .starts_with("Seen 5 more times since this issue was last updated, 6 in all"),
        "{}",
        comments[0].1
    );

    // Somebody closes it, and the fault comes back.
    tracker.state().open = false;
    age(&mut connection, &first.fingerprint, 61).await;
    record(&mut connection, &fault(7))
        .await
        .expect("a recurrence must record");
    filer
        .file(queued_jobs(&mut connection).await.remove(0))
        .await
        .expect("the recheck must succeed");

    let created = tracker.state().created.clone();
    assert_eq!(
        created.len(),
        2,
        "a recurrence after a close is a fresh issue"
    );
    assert!(created[1].body.contains("came back after #101 was closed"));
    assert_eq!(row(&mut connection, &first.fingerprint).await.1, Some(102));
}

/// What the fake GitHub saw, and the credentials' record of how they fared.
#[derive(Debug, Default)]
struct GitHubLog {
    requests: Vec<(String, Option<String>, JsonValue)>,
    usage: Vec<String>,
}

/// Credentials that hand out one token, or none, and remember their usage.
#[derive(Debug)]
struct Recorded {
    token: Option<&'static str>,
    log: Arc<Mutex<GitHubLog>>,
}

impl Credentials for Recorded {
    fn target(&self) -> CredentialsFuture<'_, Result<Option<Target>, TrackerError>> {
        Box::pin(async move {
            Ok(self.token.map(|token| {
                Target::new("mooreslabaiv1/benchmark", token).expect("a valid repository")
            }))
        })
    }

    fn record<'a>(&'a self, usage: Usage<'a>) -> CredentialsFuture<'a, ()> {
        Box::pin(async move {
            let entry = match usage {
                Usage::Accepted => "accepted".to_owned(),
                Usage::Refused(reason) => format!("refused: {reason}"),
            };
            self.log.lock().expect("unpoisoned").usage.push(entry);
        })
    }
}

/// A GitHub on loopback that accepts one token and refuses every other.
async fn fake_github(log: Arc<Mutex<GitHubLog>>) -> String {
    async fn issues(
        State(log): State<Arc<Mutex<GitHubLog>>>,
        headers: HeaderMap,
        body: String,
    ) -> (StatusCode, String) {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let version = headers
            .get("x-github-api-version")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let parsed: JsonValue = serde_json::from_str(&body).unwrap_or_default();
        log.lock()
            .expect("unpoisoned")
            .requests
            .push((version, authorization.clone(), parsed));

        if authorization.as_deref() != Some("Bearer github_pat_good") {
            return (
                StatusCode::UNAUTHORIZED,
                r#"{"message":"Bad credentials"}"#.to_owned(),
            );
        }
        (
            StatusCode::CREATED,
            r#"{"number":5,"html_url":"https://github.com/mooreslabaiv1/benchmark/issues/5","state":"open"}"#
                .to_owned(),
        )
    }

    let app = Router::new()
        .route("/repos/mooreslabaiv1/benchmark/issues", post(issues))
        .with_state(log);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback must bind");
    let address = listener.local_addr().expect("a bound address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("the fake GitHub serves");
    });
    format!("http://{address}")
}

fn new_issue() -> NewIssue {
    NewIssue {
        title: "[server] boom".to_owned(),
        body: "body".to_owned(),
        labels: vec!["bug".to_owned()],
    }
}

#[tokio::test]
async fn github_files_with_the_token_and_records_how_it_fared() {
    let log = Arc::new(Mutex::new(GitHubLog::default()));
    let api_root = fake_github(Arc::clone(&log)).await;

    let good = GitHub::with_api_root(
        Arc::new(Recorded {
            token: Some("github_pat_good"),
            log: Arc::clone(&log),
        }),
        &api_root,
    );
    let filed = good.create(&new_issue()).await.expect("a good token files");
    assert_eq!(filed.number, 5);
    assert_eq!(
        filed.url,
        "https://github.com/mooreslabaiv1/benchmark/issues/5"
    );

    let revoked = GitHub::with_api_root(
        Arc::new(Recorded {
            token: Some("github_pat_revoked"),
            log: Arc::clone(&log),
        }),
        &api_root,
    );
    let refusal = revoked
        .create(&new_issue())
        .await
        .expect_err("a revoked token is refused");
    assert!(refusal.is_refused());

    let log = log.lock().expect("unpoisoned");
    let (version, authorization, body) = &log.requests[0];
    assert_eq!(version, "2022-11-28", "every request pins the API version");
    assert_eq!(authorization.as_deref(), Some("Bearer github_pat_good"));
    assert_eq!(body["title"], "[server] boom");
    assert_eq!(body["labels"][0], "bug");

    assert_eq!(log.usage[0], "accepted");
    assert!(log.usage[1].starts_with("refused: GitHub refused the token (401)"));
    assert!(
        log.usage.iter().all(|entry| !entry.contains("github_pat")),
        "no record of a refusal may quote the token: {:?}",
        log.usage
    );
}

#[tokio::test]
async fn github_with_no_token_calls_nobody() {
    let log = Arc::new(Mutex::new(GitHubLog::default()));
    let api_root = fake_github(Arc::clone(&log)).await;

    let unconfigured = GitHub::with_api_root(
        Arc::new(Recorded {
            token: None,
            log: Arc::clone(&log),
        }),
        &api_root,
    );
    let error = unconfigured
        .create(&new_issue())
        .await
        .expect_err("nothing configured files nothing");

    assert!(error.is_not_configured());
    let log = log.lock().expect("unpoisoned");
    assert!(log.requests.is_empty(), "GitHub must not be called");
    assert!(log.usage.is_empty());
}

/// A handler that answers 500 from an unexpected state without logging why.
async fn unexplained() -> Result<&'static str, anubis::http::ApiError> {
    Err(anubis::http::ApiError::internal())
}

/// A handler that panics, which the framework must answer rather than drop.
async fn panicking() -> &'static str {
    panic!("the handler fell over");
}

#[tokio::test]
async fn a_500_nobody_logged_is_reported_and_a_panic_answers_500() {
    let Some((_database, pool)) = database("reporting_flow instrument").await else {
        return;
    };

    let (reporter, inbox) = Reporter::new(Source::Server);
    let app = anubis::reporting::instrument(
        Router::new()
            .route("/entries/{id}", get(unexplained))
            .route("/explode", get(panicking)),
        reporter.clone(),
    );

    for path in ["/entries/1", "/entries/2", "/explode"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("a request"))
            .await
            .expect("the router answers");
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{path}"
        );
    }

    // Drain what was reported, then stop.
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let draining = tokio::spawn(inbox.run(pool.clone(), async move {
        let _stopped = stopped.await;
    }));
    let _sent = stop.send(());
    draining.await.expect("the inbox drains");

    let mut connection = pool.get().await.expect("a connection");
    let faults: Vec<(String, String, i64)> = error_reports::table
        .select((
            error_reports::kind,
            error_reports::title,
            error_reports::occurrences,
        ))
        .load(&mut connection)
        .await
        .expect("the reports are readable");

    assert_eq!(
        faults,
        vec![(
            "http.request.failed".to_owned(),
            "[server] GET /entries/{id} answered 500 without logging why".to_owned(),
            2,
        )],
        "two requests to one route are one fault, and the panic is the panic hook's to report",
    );
}
