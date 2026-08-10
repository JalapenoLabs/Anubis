//! Profile, credential changes, sessions, and account deletion against a
//! real Postgres database.
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

fn extract_token(email_body: &str) -> String {
    let (_before, rest) = email_body
        .split_once("token=")
        .expect("the email must contain an action link");
    rest.split_whitespace()
        .next()
        .expect("the token must end at whitespace")
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn profile_credentials_sessions_and_deletion() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping profile_flow test: DATABASE_URL is not set");
        return;
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
    let router = Router::new().nest("/auth", anubis::auth::router(pool, mailer, &config));

    let run = Uuid::new_v4();
    let email = format!("profile-{run}@example.com");
    let password = "correct horse battery staple";

    let credentials = json!({ "email": email, "password": password });
    let (status, headers, _body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let first_session = session_token(&headers);

    // ------------------------------------------------------------------
    // Profile: names, time zone, and locale round-trip; blank clears.
    // ------------------------------------------------------------------
    let profile =
        json!({ "first_name": "  Alex ", "last_name": "Navarro", "time_zone": "America/Denver" });
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        "/auth/profile",
        Some(&profile),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["first_name"], json!("Alex"));
    assert_eq!(body["user"]["last_name"], json!("Navarro"));
    assert_eq!(body["user"]["time_zone"], json!("America/Denver"));
    assert_eq!(body["user"]["locale"], json!("en-US"), "default preserved");

    let clear = json!({ "last_name": "" });
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        "/auth/profile",
        Some(&clear),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["first_name"], json!("Alex"), "unchanged");
    assert_eq!(body["user"]["last_name"], Value::Null, "cleared");

    // ------------------------------------------------------------------
    // Password change: wrong current rejected; other sessions die.
    // ------------------------------------------------------------------
    let (status, headers, _body) =
        send(&router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK);
    let second_session = session_token(&headers);

    let wrong =
        json!({ "current_password": "not the password", "new_password": "a brand new passphrase" });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/change-password",
        Some(&wrong),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let new_password = "a brand new passphrase";
    let change = json!({ "current_password": password, "new_password": new_password });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/change-password",
        Some(&change),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, _body) =
        send(&router, "GET", "/auth/me", None, Some(&first_session)).await;
    assert_eq!(status, StatusCode::OK, "the changing session survives");
    let (status, _headers, _body) =
        send(&router, "GET", "/auth/me", None, Some(&second_session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "other sessions die");

    // ------------------------------------------------------------------
    // Email change: confirmation goes to the new address; swap on confirm.
    // ------------------------------------------------------------------
    let new_email = format!("renamed-{run}@example.com");
    let request = json!({ "new_email": new_email, "password": new_password });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/change-email/request",
        Some(&request),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");

    let change_email = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == new_email)
        .expect("the confirmation must go to the new address");
    let change_token = extract_token(&change_email.text_body);

    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/change-email/confirm",
        Some(&json!({ "token": change_token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(new_email));
    assert_eq!(body["user"]["email_verified"], json!(true));

    // ------------------------------------------------------------------
    // Sessions: list marks the current one; revocation kills the other.
    // ------------------------------------------------------------------
    let (status, headers, _body) = send(
        &router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": new_email, "password": new_password })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let third_session = session_token(&headers);

    let (status, _headers, body) =
        send(&router, "GET", "/auth/sessions", None, Some(&first_session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let listed = body["sessions"].as_array().expect("sessions must list");
    assert_eq!(listed.len(), 2, "body: {body}");
    let current = listed
        .iter()
        .find(|entry| entry["current"] == json!(true))
        .expect("the current session must be marked");
    let other = listed
        .iter()
        .find(|entry| entry["current"] == json!(false))
        .expect("the other session must be listed");
    assert_ne!(current["id"], other["id"]);

    let other_id = other["id"].as_str().expect("session id");
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &format!("/auth/sessions/{other_id}"),
        None,
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _body) =
        send(&router, "GET", "/auth/me", None, Some(&third_session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "revoked session dies");

    // ------------------------------------------------------------------
    // Account deletion: password-confirmed, then nothing works.
    // ------------------------------------------------------------------
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/account",
        Some(&json!({ "password": "wrong" })),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/account",
        Some(&json!({ "password": new_password })),
        Some(&first_session),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": new_email, "password": new_password })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the account is gone");
}
