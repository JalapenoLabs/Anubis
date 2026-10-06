//! What a refused sign-in tells the person who was refused: nothing.
//!
//! Every way a sign-in can be wrong answers with one status and one sentence:
//! a password shorter than sign-up allows, an address that is not an address,
//! an address nobody holds, and the wrong password for one somebody does. A
//! refusal that varied would tell somebody guessing which guesses are worth
//! making, or which addresses are worth guessing for.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use anubis::db::DbPool;
use anubis::mail::TestOutbox;

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }

    let request = match body {
        Some(value) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body must serialize"),
            )),
        None => builder.body(Body::empty()),
    }
    .expect("request must build");

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

#[expect(dead_code, reason = "shared harness; this file signs nobody in")]
fn session_token(headers: &HeaderMap) -> String {
    for value in headers.get_all(SET_COOKIE) {
        if let Some(rest) = value
            .to_str()
            .ok()
            .and_then(|rendered| rendered.strip_prefix("anubis_session="))
        {
            let token = rest.split(';').next().unwrap_or_default();
            if !token.is_empty() {
                return token.to_owned();
            }
        }
    }
    panic!("no session cookie in response");
}

/// The auth router on a migrated database, or `None` when none is configured.
async fn harness() -> Option<(Router, DbPool, TestOutbox)> {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping login_refusal_flow: DATABASE_URL is not set");
        return None;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
    let router = Router::new().nest(
        "/auth",
        anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
    );

    Some((router, pool, outbox))
}

#[tokio::test]
async fn every_refused_sign_in_reads_the_same() {
    let Some((router, _pool, _outbox)) = harness().await else {
        return;
    };

    let email = format!("refused-{}@example.com", Uuid::new_v4());
    let registration = json!({
        "email": email,
        "password": "correct horse battery staple",
    });
    let (status, _headers, body) =
        send(&router, "POST", "/auth/register", Some(&registration), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let attempts = [
        (
            "a password shorter than sign-up allows",
            email.clone(),
            "x".to_owned(),
        ),
        (
            "an address that is not one",
            "not-an-address".to_owned(),
            "x".to_owned(),
        ),
        (
            "an address nobody holds",
            format!("nobody-{}@example.com", Uuid::new_v4()),
            "correct horse battery staple".to_owned(),
        ),
        (
            "the wrong password",
            email.clone(),
            "wrong horse battery staple".to_owned(),
        ),
        (
            "a password over the ceiling",
            email.clone(),
            "x".repeat(513),
        ),
    ];

    let mut answers = Vec::new();
    for (case, address, password) in attempts {
        let attempt = json!({ "email": address, "password": password });
        let (status, _headers, body) =
            send(&router, "POST", "/auth/login", Some(&attempt), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}: {body}");
        answers.push(body);
    }

    let first = &answers[0];
    for answer in &answers {
        assert_eq!(answer, first, "every refusal has to read the same");
    }
}
