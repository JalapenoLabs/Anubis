//! Passwordless email sign-in codes against a real Postgres database.
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

fn extract_code(email_body: &str) -> String {
    let (_before, rest) = email_body
        .split_once("code is ")
        .expect("the email must contain the code");
    rest.split_whitespace()
        .next()
        .expect("the code must end at whitespace")
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn email_codes_sign_in_and_respect_limits() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping email_code_flow test: DATABASE_URL is not set");
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
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
    let router = Router::new().nest(
        "/auth",
        anubis::auth::router(pool, mailer, &config, &rate_limit),
    );

    let email = format!("codes-{}@example.com", Uuid::new_v4());
    let credentials = json!({ "email": email, "password": "correct horse battery staple" });
    let (status, _headers, _body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED);

    // ------------------------------------------------------------------
    // Requests answer identically for known and unknown emails.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/email-code/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let known_message = body["message"].clone();

    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/email-code/request",
        Some(&json!({ "email": "stranger@example.com" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["message"], known_message);
    assert!(
        !outbox
            .emails()
            .iter()
            .any(|sent| sent.to == "stranger@example.com"),
        "strangers get no mail"
    );

    let code_email = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("sign-in code"))
        .expect("the code email must be in the outbox");
    let code = extract_code(&code_email.text_body);
    assert_eq!(code.len(), 6);

    // ------------------------------------------------------------------
    // Wrong codes are limited; the right code signs in and is single-use.
    // ------------------------------------------------------------------
    let wrong = json!({ "email": email, "code": "000000" });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/email-code/verify",
        Some(&wrong),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let right = json!({ "email": email, "code": code });
    let (status, headers, body) = send(
        &router,
        "POST",
        "/auth/email-code/verify",
        Some(&right),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));
    assert_eq!(body["user"]["email_verified"], json!(true), "inbox proven");
    let session = session_token(&headers);
    let (status, _headers, _body) = send(&router, "GET", "/auth/me", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/email-code/verify",
        Some(&right),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "codes are single-use");

    // ------------------------------------------------------------------
    // Five wrong attempts kill an outstanding code.
    // ------------------------------------------------------------------
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/email-code/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let burned_email = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("sign-in code"))
        .expect("the fresh code email must be in the outbox");
    let burned_code = extract_code(&burned_email.text_body);

    for _attempt in 0..5 {
        let (status, _headers, _body) = send(
            &router,
            "POST",
            "/auth/email-code/verify",
            Some(&wrong),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let after_burn = json!({ "email": email, "code": burned_code });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/email-code/verify",
        Some(&after_burn),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the code dies after five wrong attempts"
    );
}
