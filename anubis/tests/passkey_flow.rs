//! Passkey ceremony endpoints against a real Postgres database.
//!
//! A real authenticator cannot run in tests, so this covers everything up to
//! the cryptographic finish: challenge issuance, state-token lifecycle,
//! garbage rejection, and credential management.
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn passkey_ceremonies_issue_challenges_and_reject_garbage() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping passkey_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    // Passkeys need a host-named origin; localhost is the dev convention.
    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("http://localhost:3000".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool, mailer, &config));

    let email = format!("passkey-{}@example.com", Uuid::new_v4());
    let credentials = json!({ "email": email, "password": "correct horse battery staple" });
    let (status, headers, _body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = session_token(&headers);

    // ------------------------------------------------------------------
    // Registration start: browser creation options plus a state token.
    // ------------------------------------------------------------------
    let (status, _headers, _body) =
        send(&router, "POST", "/auth/passkeys/register/start", None, None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "registration is signed-in"
    );

    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/passkeys/register/start",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let state_token = body["state_token"]
        .as_str()
        .expect("state token")
        .to_owned();
    let challenge = &body["creation_options"]["publicKey"];
    assert!(challenge["challenge"].is_string(), "body: {body}");
    assert_eq!(challenge["rp"]["id"], json!("localhost"), "body: {body}");
    assert_eq!(challenge["user"]["name"], json!(email), "body: {body}");

    // A garbage credential is rejected and consumes the state token.
    let garbage = json!({
        "state_token": state_token,
        "credential": {
            "id": "AAAA",
            "rawId": "AAAA",
            "type": "public-key",
            "extensions": {},
            "response": {
                "attestationObject": "AAAA",
                "clientDataJSON": "AAAA"
            }
        }
    });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/passkeys/register/finish",
        Some(&garbage),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/passkeys/register/finish",
        Some(&garbage),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "state tokens are single-use"
    );

    // ------------------------------------------------------------------
    // Login start needs no session; finish rejects unknown assertions.
    // ------------------------------------------------------------------
    let (status, _headers, body) =
        send(&router, "POST", "/auth/passkeys/login/start", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let login_state = body["state_token"]
        .as_str()
        .expect("state token")
        .to_owned();
    assert!(
        body["request_options"]["publicKey"]["challenge"].is_string(),
        "body: {body}"
    );

    let forged = json!({
        "state_token": login_state,
        "credential": {
            "id": "AAAA",
            "rawId": "AAAA",
            "type": "public-key",
            "extensions": {},
            "response": {
                "authenticatorData": "AAAA",
                "clientDataJSON": "AAAA",
                "signature": "AAAA",
                "userHandle": null
            }
        }
    });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/auth/passkeys/login/finish",
        Some(&forged),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ------------------------------------------------------------------
    // Credential management: empty list; deleting a ghost is 404.
    // ------------------------------------------------------------------
    let (status, _headers, body) =
        send(&router, "GET", "/auth/passkeys", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["passkeys"], json!([]));

    let ghost = Uuid::new_v4();
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &format!("/auth/passkeys/{ghost}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
