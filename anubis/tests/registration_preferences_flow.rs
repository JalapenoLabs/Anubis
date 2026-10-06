//! Preferences captured at sign-up, and the cooldown on resending verification.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, RETRY_AFTER, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::{TimeDelta, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use anubis::db::DbPool;
use anubis::mail::TestOutbox;
use anubis::schema::user_tokens;

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
        eprintln!("skipping registration_preferences_flow: DATABASE_URL is not set");
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
async fn registration_keeps_the_browsers_time_zone_and_locale() {
    let Some((router, _pool, _outbox)) = harness().await else {
        return;
    };

    let registration = json!({
        "email": format!("zone-{}@example.com", Uuid::new_v4()),
        "password": "correct horse battery staple",
        "time_zone": "America/Chicago",
        "locale": "fr",
    });
    let (status, _headers, body) =
        send(&router, "POST", "/auth/register", Some(&registration), None).await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["time_zone"], json!("America/Chicago"));
    assert_eq!(body["user"]["locale"], json!("fr"));
}

#[tokio::test]
async fn registration_without_preferences_takes_the_defaults() {
    let Some((router, _pool, _outbox)) = harness().await else {
        return;
    };

    let registration = json!({
        "email": format!("bare-{}@example.com", Uuid::new_v4()),
        "password": "correct horse battery staple",
    });
    let (status, _headers, body) =
        send(&router, "POST", "/auth/register", Some(&registration), None).await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["time_zone"], json!("UTC"));
    assert_eq!(body["user"]["locale"], json!("en-US"));
}

#[tokio::test]
async fn an_unusable_preference_is_dropped_and_never_refuses_the_sign_up() {
    let Some((router, _pool, _outbox)) = harness().await else {
        return;
    };

    let registration = json!({
        "email": format!("odd-{}@example.com", Uuid::new_v4()),
        "password": "correct horse battery staple",
        "time_zone": "   ",
        "locale": "x".repeat(101),
    });
    let (status, _headers, body) =
        send(&router, "POST", "/auth/register", Some(&registration), None).await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["time_zone"], json!("UTC"));
    assert_eq!(body["user"]["locale"], json!("en-US"));
}

#[tokio::test]
async fn a_verification_resend_waits_out_the_cooldown_from_the_last_link() {
    let Some((router, pool, outbox)) = harness().await else {
        return;
    };

    let email = format!("resend-{}@example.com", Uuid::new_v4());
    let registration = json!({
        "email": email,
        "password": "correct horse battery staple",
    });
    let (status, headers, body) =
        send(&router, "POST", "/auth/register", Some(&registration), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session = session_token(&headers);
    let user_id: Uuid = body["user"]["id"]
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .expect("registration must return the user id");

    // Registration just sent a link, so an immediate resend is refused and
    // told how long is left, which is never more than the whole cooldown.
    let (status, headers, _body) = send(
        &router,
        "POST",
        "/auth/verify-email/request",
        None,
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let retry_after: u64 = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .expect("a refused resend must say when to retry");
    assert!(
        (1..=300).contains(&retry_after),
        "retry after {retry_after}"
    );

    let links_sent = |outbox: &TestOutbox| {
        outbox
            .emails()
            .into_iter()
            .filter(|sent| sent.to == email && sent.subject.contains("Verify"))
            .count()
    };
    assert_eq!(links_sent(&outbox), 1, "a refused resend must send nothing");

    // Once the last link is older than the cooldown, a resend goes out.
    let mut connection = pool.get().await.expect("a connection must check out");
    diesel::update(user_tokens::table.filter(user_tokens::user_id.eq(user_id)))
        .set(user_tokens::created_at.eq(Utc::now() - TimeDelta::minutes(6)))
        .execute(&mut connection)
        .await
        .expect("the token must age");

    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/verify-email/request",
        None,
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(links_sent(&outbox), 2);

    // And that resend restarts the clock.
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/verify-email/request",
        None,
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}
