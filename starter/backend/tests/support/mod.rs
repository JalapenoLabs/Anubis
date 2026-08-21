//! Shared plumbing for the application's integration tests.
//!
//! Every model's narrative needs the same five things: a router wired to a
//! real database, a way to send a JSON request, an account, a team, and a
//! platform application's bearer token for the `/api/v1` half. They live here
//! so each `tests/<models>_flow.rs` file reads as the story of one model's
//! slice, which is exactly what `anubis scaffold model` stamps out.
//!
//! This module is application code, not a living template: the scaffolder
//! never rewrites it.
//!
//! It compiles into every test binary, and each narrative uses the parts its
//! own story needs: one mints a bearer token, another never signs anybody in.
//! So the whole module opts out of the dead-code lint rather than annotating
//! the helpers one at a time as narratives come and go.
#![allow(
    dead_code,
    reason = "every narrative compiles this module and uses the part it needs"
)]

use anubis::billing::PlanSet;
use anubis::mail::TestOutbox;
use anubis::roles::RoleSet;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
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
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config, &rate_limit),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(
                pool.clone(),
                mailer,
                roles.clone(),
                // The application's own plans, so a narrative meets the same
                // seats limit a customer does.
                Some(PlanSet::from_yaml(anubis_starter::BILLING_YML).expect("billing.yml parses")),
                &config,
                &rate_limit,
            ),
        )
        // Where a narrative mints the bearer token its `/api/v1` half uses,
        // and where it subscribes the webhook endpoint whose deliveries prove
        // the model emits its events.
        .nest(
            "/developers",
            anubis::api::management::router(pool.clone(), roles.clone()),
        )
        .nest(
            "/developers",
            anubis::webhooks::router(pool.clone(), roles.clone(), &config),
        )
        .nest(
            "/api/v1",
            anubis::api::v1::router_with(
                pool.clone(),
                anubis_starter::api_v1_router(&pool, &roles),
                anubis_starter::openapi(),
            ),
        )
        .nest("/account", anubis_starter::account_router(&pool, &roles))
        // The team's audit log, which is where a narrative reads back what its
        // writes recorded.
        .nest(
            "/account",
            anubis::audit::router(pool.clone(), roles.clone()),
        )
        // Where a provider's events arrive, with no session and no token.
        .nest("/webhooks", anubis_starter::webhooks_router(&pool))
        .layer(anubis::guard::layer(pool, roles));

    Some((router, outbox))
}

/// A connection pool onto the database [`boot`] built its router against.
///
/// A received webhook is stored for the application to process, not published,
/// so the narrative that proves it reads the rows directly. Every other
/// narrative drives the HTTP surface alone.
pub async fn pool() -> anubis::db::DbPool {
    let database_url = std::env::var("DATABASE_URL").expect("boot() already required one");
    anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable")
}

/// Sends one JSON request as a signed-in user, or as nobody.
pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _headers, value) =
        send_full(router, method, path, body, session_cookie, None).await;
    (status, value)
}

/// Sends one JSON request as a platform application's bearer token.
///
/// The `/api/v1` half of every model's narrative goes through here: the token
/// is the whole identity, so no session cookie rides along.
pub async fn send_as_token(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _headers, value) = send_full(router, method, path, body, None, token).await;
    (status, value)
}

/// Sends one JSON request, keeping the response headers.
async fn send_full(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }
    if let Some(token) = bearer {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
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
    let (status, headers, body) = send_full(
        router,
        "POST",
        "/auth/register",
        Some(&credentials),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// Creates a platform application in `team_id` and returns its bearer token.
///
/// The token is shown exactly once, at creation, which is why it is read out
/// of this response and carried through the rest of the narrative.
pub async fn platform_token(router: &Router, admin_cookie: &str, team_id: &str) -> String {
    let (status, body) = send(
        router,
        "POST",
        &format!("/developers/teams/{team_id}/platform-applications"),
        Some(&json!({ "name": "Integration suite" })),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    body["token"]
        .as_str()
        .expect("the created application carries its token exactly once")
        .to_owned()
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
