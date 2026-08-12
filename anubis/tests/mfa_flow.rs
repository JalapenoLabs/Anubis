//! TOTP enrollment, login challenge, and recovery codes against a real
//! Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use anubis::schema::{user_mfa, users};
use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
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

fn no_session_cookie(headers: &HeaderMap) -> bool {
    !headers.get_all(SET_COOKIE).iter().any(|value| {
        value
            .to_str()
            .is_ok_and(|v| v.starts_with("anubis_session="))
    })
}

fn current_code(secret: &str) -> String {
    let now = u64::try_from(Utc::now().timestamp()).expect("sane clock");
    anubis::auth::totp::code_at(secret, now).expect("the secret must decode")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn totp_gates_login_and_recovery_codes_work() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping mfa_flow test: DATABASE_URL is not set");
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
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool.clone(), mailer, &config));

    let email = format!("mfa-{}@example.com", Uuid::new_v4());
    let password = "correct horse battery staple";
    let credentials = json!({ "email": email, "password": password });

    let (status, headers, _body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = session_token(&headers);

    // ------------------------------------------------------------------
    // Enrollment: setup, QR, confirm with a real code, recovery codes.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(&router, "GET", "/auth/mfa", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["totp_enabled"], json!(false));

    let (status, _headers, body) =
        send(&router, "POST", "/auth/mfa/totp/setup", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let secret = body["secret"].as_str().expect("secret").to_owned();
    assert!(
        body["otpauth_uri"]
            .as_str()
            .is_some_and(|uri| uri.starts_with("otpauth://totp/")),
        "body: {body}"
    );

    let (status, headers, _body) =
        send(&router, "GET", "/auth/mfa/totp/qr.svg", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(CONTENT_TYPE)
            .map(axum::http::HeaderValue::as_bytes),
        Some(b"image/svg+xml".as_slice())
    );

    let wrong = json!({ "code": "000000" });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/mfa/totp/confirm",
        Some(&wrong),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a wrong code cannot confirm"
    );

    let confirm = json!({ "code": current_code(&secret) });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/mfa/totp/confirm",
        Some(&confirm),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let recovery_codes: Vec<String> = body["recovery_codes"]
        .as_array()
        .expect("recovery codes")
        .iter()
        .filter_map(|code| code.as_str().map(str::to_owned))
        .collect();
    assert_eq!(recovery_codes.len(), 10);

    let (status, _headers, body) = send(&router, "GET", "/auth/mfa", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["totp_enabled"], json!(true));

    // The seed is sealed at rest: the row holds a versioned ciphertext, never
    // the base32 secret the authenticator app scanned.
    let mut connection = pool.get().await.expect("connection must be available");
    let user_id: Uuid = users::table
        .filter(users::email.eq(&email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the user must exist");
    let stored: String = user_mfa::table
        .find(user_id)
        .select(user_mfa::totp_secret)
        .first(&mut connection)
        .await
        .expect("the enrollment must exist");
    drop(connection);
    assert!(stored.starts_with("v1:"), "got: {stored}");
    assert!(!stored.contains(&secret), "the seed must never be at rest");

    // ------------------------------------------------------------------
    // Login now answers with a challenge instead of a session.
    // ------------------------------------------------------------------
    let (status, headers, body) =
        send(&router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mfa_required"], json!(true), "body: {body}");
    assert!(no_session_cookie(&headers), "no session before the code");
    let challenge = body["mfa_token"].as_str().expect("mfa_token").to_owned();

    // A wrong code fails but does not burn the challenge.
    let wrong = json!({ "mfa_token": challenge, "code": "000000" });
    let (status, _headers, _body) =
        send(&router, "POST", "/auth/mfa/verify", Some(&wrong), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let right = json!({ "mfa_token": challenge, "code": current_code(&secret) });
    let (status, headers, body) =
        send(&router, "POST", "/auth/mfa/verify", Some(&right), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));
    let mfa_session = session_token(&headers);
    let (status, _headers, _body) =
        send(&router, "GET", "/auth/me", None, Some(&mfa_session)).await;
    assert_eq!(status, StatusCode::OK);

    // ------------------------------------------------------------------
    // Recovery codes work exactly once.
    // ------------------------------------------------------------------
    let (status, _headers, body) =
        send(&router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK);
    let challenge = body["mfa_token"].as_str().expect("mfa_token").to_owned();

    let with_recovery = json!({ "mfa_token": challenge, "code": recovery_codes[0] });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/mfa/verify",
        Some(&with_recovery),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a recovery code signs in");

    let (status, _headers, body) =
        send(&router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK);
    let challenge = body["mfa_token"].as_str().expect("mfa_token").to_owned();
    let reused = json!({ "mfa_token": challenge, "code": recovery_codes[0] });
    let (status, _headers, _body) =
        send(&router, "POST", "/auth/mfa/verify", Some(&reused), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "recovery codes are single-use"
    );

    // ------------------------------------------------------------------
    // Disabling restores plain password login.
    // ------------------------------------------------------------------
    let disable = json!({ "password": password });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/mfa/totp/disable",
        Some(&disable),
        Some(&mfa_session),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, headers, body) =
        send(&router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["user"]["email"],
        json!(email),
        "a direct session again"
    );
    let _direct_session = session_token(&headers);
}
