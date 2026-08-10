//! Platform applications and the v1 API against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use anubis::schema::{team_memberships, teams, users};

const ROLES_YML: &str = "
roles:
  default:
    models: {}
  editor:
    includes: [default]
    models: {}
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [editor, billing]
    models: {}
";

struct TestRequest<'a> {
    method: &'a str,
    path: &'a str,
    body: Option<&'a Value>,
    session_cookie: Option<&'a str>,
    bearer: Option<&'a str>,
}

async fn send(router: &Router, request: TestRequest<'_>) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(request.method).uri(request.path);
    if let Some(cookie) = request.session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }
    if let Some(token) = request.bearer {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }

    let built = match request.body {
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
        .oneshot(built)
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
async fn platform_tokens_authenticate_the_v1_api() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping api_flow test: DATABASE_URL is not set");
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
    let roles = anubis::roles::RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();

    let router = Router::new()
        .nest("/auth", anubis::auth::router(pool.clone(), mailer, &config))
        .nest(
            "/developers",
            anubis::api::management::router(pool.clone(), roles),
        )
        .nest("/api/v1", anubis::api::v1::router(pool.clone()));

    let run = Uuid::new_v4();
    let admin_email = format!("api-admin-{run}@example.com");
    let outsider_email = format!("api-outsider-{run}@example.com");

    let credentials = json!({ "email": admin_email, "password": "correct horse battery staple" });
    let (status, headers, _body) = send(
        &router,
        TestRequest {
            method: "POST",
            path: "/auth/register",
            body: Some(&credentials),
            session_cookie: None,
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let admin_cookie = session_token(&headers);

    let outsider_credentials =
        json!({ "email": outsider_email, "password": "correct horse battery staple" });
    let (status, headers, _body) = send(
        &router,
        TestRequest {
            method: "POST",
            path: "/auth/register",
            body: Some(&outsider_credentials),
            session_cookie: None,
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let outsider_cookie = session_token(&headers);

    let mut connection = pool.get().await.expect("connection must be available");
    let admin_id: Uuid = users::table
        .filter(users::email.eq(&admin_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the admin must exist");
    let team_id: Uuid = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(admin_id))),
        )
        .select(teams::id)
        .first(&mut connection)
        .await
        .expect("the bootstrapped team must exist");

    let applications_path = format!("/developers/teams/{team_id}/platform-applications");

    // ------------------------------------------------------------------
    // Create an application: the token is returned exactly once.
    // ------------------------------------------------------------------
    let create = json!({ "name": "Integration suite" });
    let (status, _headers, body) = send(
        &router,
        TestRequest {
            method: "POST",
            path: &applications_path,
            body: Some(&create),
            session_cookie: Some(&admin_cookie),
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let bearer = body["token"]
        .as_str()
        .expect("token must be returned")
        .to_owned();
    let application_id = body["application"]["id"]
        .as_str()
        .expect("application id must be returned")
        .to_owned();

    // Listing shows metadata, never tokens.
    let (status, _headers, body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: &applications_path,
            body: None,
            session_cookie: Some(&admin_cookie),
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["applications"][0]["name"], json!("Integration suite"));
    assert!(
        !body.to_string().contains(&bearer),
        "raw tokens must never be listed"
    );

    // An outsider is not a member: the guard answers 404.
    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: &applications_path,
            body: None,
            session_cookie: Some(&outsider_cookie),
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // ------------------------------------------------------------------
    // The bearer token authenticates the v1 API and scopes it to the team.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: Some(&bearer),
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"]["id"], json!(team_id.to_string()));
    assert_eq!(body["team"]["name"], json!("General"));

    // Missing and garbage tokens are 401.
    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: Some("forged-token"),
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ------------------------------------------------------------------
    // Rotation kills the old token; the new one works.
    // ------------------------------------------------------------------
    let rotate_path =
        format!("/developers/teams/{team_id}/platform-applications/{application_id}/rotate-token");
    let (status, _headers, body) = send(
        &router,
        TestRequest {
            method: "POST",
            path: &rotate_path,
            body: None,
            session_cookie: Some(&admin_cookie),
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let rotated = body["token"].as_str().expect("rotated token").to_owned();
    assert_ne!(rotated, bearer);

    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: Some(&bearer),
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "old token must be dead");
    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: Some(&rotated),
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // ------------------------------------------------------------------
    // Deleting the application revokes its tokens.
    // ------------------------------------------------------------------
    let delete_path = format!("/developers/teams/{team_id}/platform-applications/{application_id}");
    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "DELETE",
            path: &delete_path,
            body: None,
            session_cookie: Some(&admin_cookie),
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/team",
            body: None,
            session_cookie: None,
            bearer: Some(&rotated),
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "deleted app tokens die");

    // ------------------------------------------------------------------
    // The OpenAPI document and docs page are served unauthenticated.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/openapi.json",
            body: None,
            session_cookie: None,
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["openapi"]
            .as_str()
            .is_some_and(|version| version.starts_with("3.1")),
        "body: {body}"
    );
    assert!(body["paths"].get("/api/v1/team").is_some(), "body: {body}");

    let (status, _headers, _body) = send(
        &router,
        TestRequest {
            method: "GET",
            path: "/api/v1/docs",
            body: None,
            session_cookie: None,
            bearer: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
