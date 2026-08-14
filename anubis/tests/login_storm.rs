//! What a burst of sign-ins costs, and what else it costs while it lasts.
//!
//! Verifying a password is argon2id, which is CPU-bound on purpose: an
//! attacker's guess has to be expensive or the hash is decoration. That makes
//! sign-in the one endpoint whose cost a stranger controls, and the question
//! this file answers is what happens to the rest of the server while a lot of
//! them arrive at once.
//!
//! `anubis::auth::password` runs both the hash and the verification through
//! `tokio::task::spawn_blocking`, so the work lands on the blocking pool
//! rather than on a runtime worker. This benchmark measures whether that
//! separation holds up: it fires [`STORM`] sign-ins at a real socket, times
//! each one, and times `GET /healthz` throughout, which touches nothing and
//! therefore should stay fast no matter how deep the login queue gets.
//!
//! Ignored by default, because it is a measurement rather than an assertion.
//! Run it deliberately:
//!
//! ```sh
//! DATABASE_URL=postgres://app:app@localhost:54321/app_development \
//! RATE_LIMIT_DISABLED=true \
//!   cargo test --release -p anubis --test login_storm -- --ignored --nocapture
//! ```
//!
//! `--release` matters more than anything else here: argon2 in a debug build
//! is roughly two orders of magnitude slower than the one production runs, so
//! debug numbers describe nothing real.
//!
//! `RATE_LIMIT_DISABLED=true` matters too, and says something about the shape
//! of the answer. In production the limiter is the *first* line: ten credential
//! attempts per client per minute, refused with a `429` long before any of this
//! is reached. What this benchmark measures is the layer behind it, which is
//! what a distributed burst across many client addresses actually meets.

mod support;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use serde_json::json;

use support::{Harness, PASSWORD, TestDatabase, register, serve};

/// How many sign-ins arrive at once.
///
/// Comfortably more than the machine has cores, so the blocking pool is
/// saturated and the queue behind it is real, and few enough that the whole
/// run finishes in seconds.
const STORM: usize = 64;

/// How often the liveness probe is sampled during the storm.
const PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// How long the probe runs before the storm, to establish a baseline.
const BASELINE: Duration = Duration::from_millis(400);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a benchmark, not an assertion: see the module docs for how to run it"]
async fn a_login_storm_does_not_starve_the_rest_of_the_server() {
    let Some(database) = TestDatabase::create("login_storm").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let address = serve(anubis::server::harden(
        harness.router.clone(),
        harness.pool.clone(),
        &Harness::config(),
    ));

    let email = "storm@example.com";
    let _registered = register(&harness.router, email).await;

    let client = reqwest::Client::builder()
        // Far above any latency the storm can produce, so a slow answer is
        // measured rather than turned into a client-side failure.
        .timeout(Duration::from_mins(1))
        // A connection per racer, so the measurement is of the server and not
        // of a client-side queue in front of it.
        .pool_max_idle_per_host(STORM)
        .build()
        .expect("the client must build");
    let login_url = format!("http://{address}/auth/login");
    let health_url = format!("http://{address}/healthz");
    // Serialized once; reqwest's `json` feature is off, so the body and its
    // content type are spelled out rather than pulling in another feature for
    // one call site.
    let credentials = json!({ "email": email, "password": PASSWORD }).to_string();

    // One sign-in first: it warms the pool and gives the cost of a single
    // uncontended verification, which everything below is compared against.
    let alone = sign_in(&client, &login_url, credentials.clone()).await.1;

    // The probe runs across the baseline and the storm alike, so the two
    // populations are measured by the same instrument.
    let (stop, stopping) = tokio::sync::watch::channel(());
    let probe = tokio::spawn(probe_liveness(client.clone(), health_url, stopping));
    tokio::time::sleep(BASELINE).await;

    let storm_started = Instant::now();
    let mut attempts = Vec::with_capacity(STORM);
    for _attempt in 0..STORM {
        let client = client.clone();
        let login_url = login_url.clone();
        let credentials = credentials.clone();
        attempts.push(tokio::spawn(async move {
            sign_in(&client, &login_url, credentials).await
        }));
    }

    let mut latencies = Vec::with_capacity(STORM);
    for attempt in attempts {
        let (status, elapsed) = attempt.await.expect("a sign-in task must not panic");
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "the rate limiter answered: rerun with RATE_LIMIT_DISABLED=true, \
             which is the layer this benchmark measures behind",
        );
        assert_eq!(status, StatusCode::OK, "every sign-in must succeed");
        latencies.push(elapsed);
    }
    let wall = storm_started.elapsed();

    // Let the probe sample a little past the storm, then ask it to stop.
    tokio::time::sleep(PROBE_INTERVAL * 4).await;
    let _stopping = stop.send(());
    let (baseline, under_load) = probe.await.expect("the probe must not panic");

    report(&Storm {
        alone,
        wall,
        latencies,
        baseline,
        under_load,
    });
}

/// Signs in once, answering the status and how long it took.
async fn sign_in(
    client: &reqwest::Client,
    url: &str,
    credentials: String,
) -> (StatusCode, Duration) {
    let started = Instant::now();
    let response = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .body(credentials)
        .send()
        .await
        .expect("a sign-in must complete");
    (response.status(), started.elapsed())
}

/// Samples `GET /healthz` until asked to stop, split at [`BASELINE`].
///
/// It stops on a signal rather than being aborted, because an aborted task's
/// samples are lost with it.
async fn probe_liveness(
    client: reqwest::Client,
    url: String,
    mut stopping: tokio::sync::watch::Receiver<()>,
) -> (Vec<Duration>, Vec<Duration>) {
    let mut baseline = Vec::new();
    let mut under_load = Vec::new();
    let started = Instant::now();

    loop {
        let sample_started = Instant::now();
        let response = client
            .get(&url)
            .send()
            .await
            .expect("the liveness probe must answer");
        let elapsed = sample_started.elapsed();
        assert_eq!(response.status(), StatusCode::OK, "healthz must stay live");

        if started.elapsed() < BASELINE {
            baseline.push(elapsed);
        } else {
            under_load.push(elapsed);
        }

        tokio::select! {
            _stopped = stopping.changed() => break,
            () = tokio::time::sleep(PROBE_INTERVAL) => {}
        }
    }

    (baseline, under_load)
}

/// Everything one run measured.
#[derive(Debug)]
struct Storm {
    /// One sign-in with nothing else happening.
    alone: Duration,
    /// How long the whole storm took, start to last answer.
    wall: Duration,
    /// One entry per sign-in.
    latencies: Vec<Duration>,
    /// Liveness probes taken before the storm.
    baseline: Vec<Duration>,
    /// Liveness probes taken during it.
    under_load: Vec<Duration>,
}

/// Prints the run as a table, with the conclusion it supports.
fn report(storm: &Storm) {
    let logins = Summary::of(&storm.latencies);
    let baseline = Summary::of(&storm.baseline);
    let under_load = Summary::of(&storm.under_load);

    println!("\n=== login storm: {STORM} concurrent sign-ins ===");
    if cfg!(debug_assertions) {
        println!(
            "WARNING: debug build. argon2 is orders of magnitude slower here \
             than in release; rerun with --release for numbers that mean anything."
        );
    }
    println!(
        "build                : {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!("parallelism          : {} cores", num_cores());
    println!("one sign-in alone    : {:>8.1?}", storm.alone);
    println!("storm wall clock     : {:>8.1?}", storm.wall);
    println!(
        "sign-in latency      : p50 {:>8.1?}  p95 {:>8.1?}  max {:>8.1?}  (n={})",
        logins.p50, logins.p95, logins.max, logins.count,
    );
    println!(
        "healthz, quiet       : p50 {:>8.1?}  p95 {:>8.1?}  max {:>8.1?}  (n={})",
        baseline.p50, baseline.p95, baseline.max, baseline.count,
    );
    println!(
        "healthz, under storm : p50 {:>8.1?}  p95 {:>8.1?}  max {:>8.1?}  (n={})",
        under_load.p50, under_load.p95, under_load.max, under_load.count,
    );
    println!(
        "throughput           : {:.1} sign-ins/second",
        f64::from(u32::try_from(STORM).unwrap_or(u32::MAX)) / storm.wall.as_secs_f64(),
    );
    println!(
        "\nRead it this way: sign-in latency rising with the queue is the \
         design working, because argon2 is meant to cost. healthz staying \
         near its quiet number is the isolation working, because the \
         verifications are on the blocking pool and the probe is on a runtime \
         worker. healthz rising with the storm would mean the two are \
         competing, and that is the finding to act on.\n"
    );
}

/// The percentiles a latency population is judged by.
#[derive(Debug, Default)]
struct Summary {
    count: usize,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

impl Summary {
    /// Summarizes a population, answering zeroes for an empty one.
    fn of(samples: &[Duration]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }

        let mut sorted = samples.to_vec();
        sorted.sort_unstable();

        Self {
            count: sorted.len(),
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            max: *sorted.last().expect("the population is not empty"),
        }
    }
}

/// The `nth` percentile of an already sorted population.
fn percentile(sorted: &[Duration], nth: usize) -> Duration {
    // Nearest-rank: the smallest value at or above which `nth` percent of the
    // population sits. Exact for small populations, which these are.
    let rank = (sorted.len() * nth).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// How many threads the runtime can actually run at once.
fn num_cores() -> usize {
    std::thread::available_parallelism().map_or(0, std::num::NonZero::get)
}
