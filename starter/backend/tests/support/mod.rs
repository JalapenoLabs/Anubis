//! Shared plumbing for the application's integration tests.
//!
//! Every model's narrative needs the same four things: a router wired to a
//! real database, a way to send a JSON request, an account, and a team. They
//! live here so each `tests/<models>_flow.rs` file reads as the story of one
//! model's slice, which is exactly what `anubis scaffold model` stamps out.
//!
//! This module is application code, not a living template: the scaffolder
//! never rewrites it.

use anubis::mail::TestOutbox;
use anubis::roles::RoleSet;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::{Router, body::Body};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Builds the application's router against the database in `DATABASE_URL`.
///
/// Returns `None` when the variable is unset, which is the signal for a test
/// to log a skip and pass. CI always provides one.
pub async fn boot() -> Option<(Router, TestOutbox)> {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return None;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("framework migrations must apply");
    anubis::db::run_app_migrations(&database_url, anubis_starter::APP_MIGRATIONS)
        .await
        .expect("application migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let roles = RoleSet::from_yaml(anubis_starter::ROLES_YML).expect("roles.yml must parse");
    let (mailer, outbox) = anubis::mail::Mailer::test();

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles.clone(), &config),
        )
        .nest("/account", anubis_starter::account_router(&pool, &roles))
        .layer(anubis::guard::layer(pool, roles));

    Some((router, outbox))
}

/// Sends one JSON request and returns the status and decoded body.
pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _headers, value) = send_full(router, method, path, body, session_cookie).await;
    (status, value)
}

/// Sends one JSON request, keeping the response headers.
async fn send_full(
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

/// Registers an account and returns its session cookie.
pub async fn register(router: &Router, email: &str) -> String {
    let credentials = json!({ "email": email, "password": "correct horse battery staple" });
    let (status, headers, body) =
        send_full(router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// The id of the team registration bootstrapped for this account.
pub async fn bootstrapped_team(router: &Router, cookie: &str) -> String {
    let (status, body) = send(router, "GET", "/tenancy/memberships", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["organizations"][0]["teams"][0]["id"]
        .as_str()
        .expect("registration bootstraps one team")
        .to_owned()
}

/// Invites `email` to `team_id` with `roles` and claims the invitation.
pub async fn invite_and_claim(
    router: &Router,
    outbox: &TestOutbox,
    admin_cookie: &str,
    joining_cookie: &str,
    team_id: &str,
    email: &str,
    roles: &[&str],
) {
    let invite = json!({ "email": email, "team_id": team_id, "roles": roles });
    let (status, body) = send(
        router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let invitation = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation
        .text_body
        .split_once("token=")
        .expect("the email must contain a link");
    let token = rest.split_whitespace().next().unwrap_or_default();

    let (status, body) = send(
        router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(joining_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// Reads the session cookie out of a response's `Set-Cookie` headers.
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
