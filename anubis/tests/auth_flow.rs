//! End-to-end auth and session flow against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes, so
//! plain `cargo test` still works on machines without a database. CI always
//! provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

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

/// Pulls the session token out of a response's Set-Cookie headers.
fn session_token(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(SET_COOKIE) {
        let rendered = value.to_str().ok()?;
        if let Some(rest) = rendered.strip_prefix("anubis_session=") {
            let token = rest.split(';').next().unwrap_or_default();
            if !token.is_empty() {
                return Some(token.to_owned());
            }
        }
    }
    None
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative; splitting it would re-register users per step"
)]
async fn register_login_and_session_round_trip() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping auth_flow test: DATABASE_URL is not set");
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
    let router = anubis::auth::router(pool, mailer, &config);
    let email = format!("it-{}@example.com", uuid::Uuid::new_v4());
    let password = "correct horse battery staple";
    let credentials = json!({ "email": email, "password": password });

    // Registration creates the account, returns the user, and signs in.
    let (status, headers, body) =
        send(&router, "POST", "/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));
    assert!(body["user"].get("password_hash").is_none(), "body: {body}");
    let register_cookie = session_token(&headers).expect("registration must set a session cookie");
    let rendered_cookie = headers
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| value.to_str().ok())
        .expect("cookie header must render");
    assert!(
        rendered_cookie.contains("HttpOnly"),
        "got: {rendered_cookie}"
    );
    assert!(
        rendered_cookie.contains("SameSite=Lax"),
        "got: {rendered_cookie}"
    );

    // The session cookie authenticates /me.
    let (status, _headers, body) = send(&router, "GET", "/me", None, Some(&register_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));

    // No cookie, garbage cookie: both are 401.
    let (status, _headers, _body) = send(&router, "GET", "/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _headers, _body) =
        send(&router, "GET", "/me", None, Some("forged-session-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The same email cannot register twice.
    let (status, _headers, body) =
        send(&router, "POST", "/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    // Logout kills the session server-side; the old cookie stops working.
    let (status, _headers, _body) =
        send(&router, "POST", "/logout", None, Some(&register_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _body) = send(&router, "GET", "/me", None, Some(&register_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Login issues a fresh working session, and email lookup is
    // case-insensitive because emails normalize on the way in.
    let upper_credentials = json!({ "email": email.to_uppercase(), "password": password });
    let (status, headers, body) =
        send(&router, "POST", "/login", Some(&upper_credentials), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let login_cookie = session_token(&headers).expect("login must set a session cookie");
    assert_ne!(login_cookie, register_cookie, "tokens must rotate");
    let (status, _headers, body) = send(&router, "GET", "/me", None, Some(&login_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // A wrong password and an unknown email fail identically.
    let wrong_password = json!({ "email": email, "password": "wrong password entirely" });
    let (status, _headers, body) =
        send(&router, "POST", "/login", Some(&wrong_password), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let wrong_password_message = body["message"].clone();

    let unknown_email = json!({ "email": "nobody@example.com", "password": password });
    let (status, _headers, body) =
        send(&router, "POST", "/login", Some(&unknown_email), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], wrong_password_message);

    // Broken input is rejected before it touches the database.
    let broken = json!({ "email": "not-an-email", "password": password });
    let (status, _headers, _body) = send(&router, "POST", "/register", Some(&broken), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ------------------------------------------------------------------
    // Email verification: registration already sent a link to the outbox.
    // ------------------------------------------------------------------
    let (_status, _headers, body) = send(&router, "GET", "/me", None, Some(&login_cookie)).await;
    assert_eq!(body["user"]["email_verified"], json!(false), "body: {body}");

    let verification_email = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == email && sent.subject.contains("Verify"))
        .expect("registration must send a verification email");
    let verify_token = extract_token(&verification_email.text_body);

    let confirm = json!({ "token": verify_token });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/verify-email/confirm",
        Some(&confirm),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email_verified"], json!(true), "body: {body}");

    // Tokens are single-use.
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/verify-email/confirm",
        Some(&confirm),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ------------------------------------------------------------------
    // Password reset: request, confirm, and verify session revocation.
    // ------------------------------------------------------------------
    let request = json!({ "email": email });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/password-reset/request",
        Some(&request),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let known_message = body["message"].clone();

    // Unknown emails get the same answer, so responses cannot probe accounts.
    let unknown_request = json!({ "email": "nobody@example.com" });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/password-reset/request",
        Some(&unknown_request),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["message"], known_message);
    let mail_to_stranger = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == "nobody@example.com");
    assert!(
        mail_to_stranger.is_none(),
        "unknown emails must not be mailed"
    );

    let reset_email = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("Reset"))
        .expect("the reset email must be in the outbox");
    let reset_token = extract_token(&reset_email.text_body);

    let new_password = "an entirely new passphrase";
    let confirm_reset = json!({ "token": reset_token, "password": new_password });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/password-reset/confirm",
        Some(&confirm_reset),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Every session died with the reset.
    let (status, _headers, _body) = send(&router, "GET", "/me", None, Some(&login_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The old password is gone; the new one works.
    let old_login = json!({ "email": email, "password": password });
    let (status, _headers, _body) = send(&router, "POST", "/login", Some(&old_login), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let new_login = json!({ "email": email, "password": new_password });
    let (status, _headers, body) = send(&router, "POST", "/login", Some(&new_login), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// Pulls the `token=` value out of an emailed action link.
fn extract_token(email_body: &str) -> String {
    let (_before, rest) = email_body
        .split_once("token=")
        .expect("the email must contain an action link");
    rest.split_whitespace()
        .next()
        .expect("the token must end at whitespace")
        .to_owned()
}
