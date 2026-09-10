//! Rate limiting driven through the composed auth router.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes, so
//! plain `cargo test` still works on machines without a database. CI always
//! provides one.
//!
//! Requests carry a `ConnectInfo` extension, which is what the server's accept
//! loop provides in production and what lets a test pick the address a request
//! is charged to.

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Instant;
use tower::ServiceExt;

async fn send(
    router: &Router,
    path: &str,
    body: &Value,
    client: &str,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("body must serialize"),
        ))
        .expect("request must build");

    let peer: SocketAddr = client.parse().expect("the client address must parse");
    request.extensions_mut().insert(ConnectInfo(peer));

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

/// Builds the real auth router with limits on, as a deployment runs it.
async fn router(database_url: &str) -> Router {
    anubis::db::run_pending_migrations(database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    anubis::auth::router(pool, mailer, &config, &rate_limit)
}

#[tokio::test]
async fn login_attempts_run_out_per_client_address() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping rate_limit_flow test: DATABASE_URL is not set");
        return;
    };
    let router = router(&database_url).await;

    let attacker = "198.51.100.20:41000";
    let bystander = "198.51.100.21:41000";
    let quota = anubis::rate_limit::Budget::Credentials.quota();

    // Nobody is registered under these addresses, so every attempt is a 401
    // until the budget runs out. That is the point: the limit counts volume,
    // not outcomes, so it says nothing about which accounts exist.
    let credentials = json!({
        "email": format!("nobody-{}@example.com", uuid::Uuid::new_v4()),
        "password": "a wrong password entirely",
    });
    // The budget leaks continuously rather than resetting on a boundary, so
    // how many attempts it admits depends on how long they take: one cell
    // returns every `period / quota`, which is six seconds here. Spending the
    // budget therefore means sending until the refusal arrives, not sending
    // exactly `quota` requests and assuming none replenished. Each attempt
    // costs a real argon2id verification, so on a loaded machine the burst
    // outlasts an emission interval and the naive count is wrong.
    let period = anubis::rate_limit::Budget::Credentials.period();
    let emission = period / quota;
    // Four budgets' worth: enough that a working limiter always refuses
    // inside it, few enough that a broken one fails rather than hangs.
    let ceiling = quota * 4;

    let start = Instant::now();
    let mut admitted = 0_u32;
    let mut refusal = None;
    for attempt in 0..ceiling {
        let (status, headers, body) = send(&router, "/login", &credentials, attacker).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            refusal = Some((headers, body));
            break;
        }
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} was admitted, so it must be a plain rejection, body: {body}",
        );
        admitted += 1;
    }

    let elapsed = start.elapsed();
    let (headers, body) = refusal.unwrap_or_else(|| {
        panic!("the budget never ran out: {admitted} attempts admitted in {elapsed:?}")
    });

    // What the leak can have returned while the burst was in flight. Asserting
    // against this rather than against `quota` alone is what makes the test
    // independent of how slow the machine is, without letting a limiter that
    // admits everything pass.
    let replenished = elapsed.as_nanos() / emission.as_nanos();
    let ceiling_for_run = u128::from(quota) + replenished;
    assert!(
        u128::from(admitted) <= ceiling_for_run,
        "admitted {admitted} in {elapsed:?}, over the budget of {quota} plus {replenished} leaked",
    );
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .expect("a 429 must say when to retry");
    let seconds: u64 = retry_after.parse().expect("Retry-After is whole seconds");
    assert!(seconds >= 1, "got: {retry_after}");
    assert!(
        body["message"].as_str().is_some_and(|message| {
            message.contains("Too many requests") && !message.contains("login")
        }),
        "the body must not name what tripped, body: {body}",
    );

    // A registered account behind the same address gets the same answer, so
    // the limit cannot be used to tell accounts apart.
    let registered = json!({ "email": "someone@example.com", "password": "another password" });
    let (status, _headers, _body) = send(&router, "/login", &registered, attacker).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // Another address is untouched: the budget is per client.
    let (status, _headers, body) = send(&router, "/login", &credentials, bystander).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

#[tokio::test]
async fn reset_emails_run_out_per_recipient_however_many_addresses_ask() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping rate_limit_flow test: DATABASE_URL is not set");
        return;
    };
    let router = router(&database_url).await;

    let victim = json!({ "email": format!("victim-{}@example.com", uuid::Uuid::new_v4()) });
    let quota = anubis::rate_limit::Budget::EmailPerRecipient.quota();

    // Every request rotates the client address, the way a mail bombing run
    // does. The recipient budget is what stops it.
    for attempt in 0..quota {
        let client = format!("203.0.113.{}:41000", attempt + 1);
        let (status, _headers, body) =
            send(&router, "/password-reset/request", &victim, &client).await;
        assert_eq!(
            status,
            StatusCode::ACCEPTED,
            "attempt {attempt} is within the budget, body: {body}",
        );
    }

    let (status, headers, body) = send(
        &router,
        "/password-reset/request",
        &victim,
        "203.0.113.200:41000",
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert!(
        headers.contains_key(RETRY_AFTER),
        "a 429 says when to retry"
    );

    // Another inbox is untouched, from an address that just tripped the limit.
    let other = json!({ "email": format!("other-{}@example.com", uuid::Uuid::new_v4()) });
    let (status, _headers, body) = send(
        &router,
        "/password-reset/request",
        &other,
        "203.0.113.200:41000",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
}
