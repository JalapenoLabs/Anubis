# Error reporting

`anubis::reporting` turns an application's own failures into issues in a tracker, **one issue per fault** however often the fault recurs. A `500`, a panic, an `ERROR` log line, a browser that crashed rendering a page, and a background task that fell over all become a `Report`; every report is counted against a fingerprint in the `error_reports` table; and a fingerprint is filed once, by a durable job, through a `Tracker` the application configures. GitHub issues is the tracker the framework ships.

Nothing here can make a failure worse. Reporting a fault never blocks, never fails the request that hit it, and never calls the tracker on a request path.

## The pieces

| Piece | What it does |
|---|---|
| `Report` | One occurrence: a source, a kind, a one-line summary, a signature, an optional subject, facts, and a detail. **Redacted and clipped as it is built**, so no unmasked text can reach the database or the tracker. |
| `record(connection, &report)` | Counts the occurrence and, in the same transaction, queues the filing job when one is due. |
| `Reporter` / `Inbox` | A cheap, non-blocking handle for code that cannot await (a panic hook, a tracing layer), and the task that drains it into `record`. |
| `Filer` | Runs `FileErrorReport` jobs against a `Tracker`. |
| `Tracker` | The seam: where issues go. Four calls: create, find an open issue carrying a fingerprint, ask whether one is open, comment. |
| `github::GitHub` | The GitHub implementation, over the REST API, asking a `Credentials` for its token on every call. |
| `ErrorLayer`, `install_panic_hook`, `instrument` | Capture: every `ERROR` event, every panic, and every `500` that logged no cause. |

## Wiring an application

```rust
use anubis::reporting::{self, Filer, FileErrorReport, Reporter, Source};

// First thing in main: reports wait in the reporter until the pool exists.
let (reporter, inbox) = Reporter::new(Source::Server);
anubis::telemetry::init_with_reporting(&config, &reporter)?;
reporting::install_panic_hook(reporter.clone());

// Once the pool exists.
tokio::spawn(inbox.run(pool.clone(), shutdown));
let app = reporting::instrument(app, reporter.clone());

// On the job worker of the one process that files.
let filer = Filer::new(pool.clone(), Arc::new(reporting::github::GitHub::new(credentials)), "My product");
worker.register(move |job: FileErrorReport| {
    let filer = filer.clone();
    async move { filer.file(job).await }
})
```

`credentials` implements `github::Credentials`: hand out a `Target` (a repository and a token) per call, and record how the token fared. An application that configures one token at boot uses `github::Fixed`; one whose operators paste a token into a settings page reads and opens it there. A process doing background work of its own creates its reporter with `Source::Worker`, records its faults, and need not register the filer: the filing job runs wherever a worker registered it.

## What is captured

- **`ERROR` events.** The framework's convention is to log a failure's cause where it happens and answer a generic `500`, so the log line is the report. The event's message template is the stable part (templates are written with escaped placeholders, `"failed: {{error.message}}"`), so the signature is the template plus the event's `error.message` (or `error`) field, normalized. The report carries the event's fields and the request span's method, path and request id.
- **Panics**, anywhere in the process, with their location, their message, and a backtrace captured regardless of `RUST_BACKTRACE`. The previous hook still runs.
- **A `500` nobody logged.** `instrument` remembers which requests logged an error and reports a `500` itself only when none did, under the matched route rather than the path. It also answers a panicking handler with the framework's `500` rather than a dropped connection.
- **Anything an application reports itself**, with `Reporter::report` or `record`.

The layer ignores the `anubis::reporting` target and the job worker's lines about `FileErrorReport`, so a tracker outage never files issues about failing to file issues.

## One issue per fault

A fingerprint is SHA-256 over the source, the kind, the subject, and the normalized signature, length-prefixed, sixteen hex characters. Normalization masks uuids (`<id>`), hex runs of eight or more (`<hash>`, which covers a bundle hash that changes every deploy) and digit runs (`<n>`). A subject is hashed as written: it is for a fault that is a different problem on each of several things, such as one credential out of several that a vendor refused.

| What happens | Then |
|---|---|
| A fault nobody has seen | A filing job is queued in the transaction that counts it |
| It recurs while its issue is open | It is counted; nothing else |
| An hour since the issue was last checked | The next occurrence queues a recheck. Still open: one comment with the occurrences since it last heard. Closed: a fresh issue naming the closed one. Closed issues are never reopened |
| The tracker is unavailable | The job fails and the queue retries it, three attempts |
| The tracker refuses | Recorded on the row with the tracker's words; retried by a later occurrence after fifteen minutes |
| Nothing is configured | Recorded as `unconfigured`; faults are still counted |
| Ten issues already filed this hour | Withheld and counted, under an advisory lock so a burst cannot all pass one count |

Counting is an upsert under the row's lock, so a burst of a new fault files once. A filing marks the row `create_unconfirmed` before the request goes out, and a later attempt that sees it searches the tracker for an open issue carrying the fingerprint in its body before creating, adopting what it finds.

## Redaction

Every text a report holds passes through `reporting::redact` as it is built: credential headers, bearer and basic credentials, secret-named fields in a query or JSON, credentials in a URL, vendor key shapes (`ghp_`, `github_pat_`, `sk-ant-`, `sk-`, `AIza`, `xox`, `AKIA`), JSON web tokens, PEM private keys, and every email address. Issue bodies render every value as a code span or a fenced block that cannot be escaped, so nothing a failure said renders as markdown or mentions anybody.

## GitHub

A fine-grained personal access token scoped to one repository with **Issues: read and write** is all `GitHub` needs. Every request pins API version `2022-11-28`. A `401`, a `404`, a `410`, and a `403` without a spent rate limit are refusals, recorded through `Credentials::record` in words an operator can act on; a `5xx`, a `429`, and a rate-limited `403` are outages the queue retries. `GitHub::with_api_root` points it at GitHub Enterprise or at a test's fake.

## The rate limit budget

`rate_limit::Budget::ErrorReports`, thirty an hour per client, is for an endpoint a browser reports through without a session. Bounding how many new fingerprints a stranger may introduce is the application's decision; `reporting::seen` and `reporting::new_since` are what it is decided with.

## Migrations

`error_reports` is a framework table, applied by `run_pending_migrations`. The framework's migrations and an application's share one version table, so a framework migration's version must never equal any application migration's version.
