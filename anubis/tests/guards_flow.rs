//! Ownership-chain guard behavior against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router, body::Body};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use anubis::guard::{OrganizationMember, TeamMember};
use anubis::http::ApiError;
use anubis::roles::Action;
use anubis::schema::{team_memberships, teams, users};

/// `default` deliberately has no Probe grants, so a claimed default member
/// exercises the 403 path.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  editor:
    includes: [default]
    models:
      Probe: [read, update]
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [editor, billing]
    models:
      Probe: [manage]
";

async fn team_probe(member: TeamMember) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Update, "Probe")?;
    Ok(Json(json!({ "team": member.team.name })))
}

async fn organization_probe(member: OrganizationMember) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Read, "Probe")?;
    Ok(Json(json!({ "organization": member.organization.name })))
}

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

async fn register(router: &Router, email: &str) -> String {
    let credentials = json!({ "email": email, "password": "correct horse battery staple" });
    let (status, headers, body) =
        send(router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn guards_enforce_membership_and_permissions() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping guards_flow test: DATABASE_URL is not set");
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
    let (mailer, outbox) = anubis::mail::Mailer::test();

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles.clone(), None, &config),
        )
        .route("/teams/{team_id}/probe", get(team_probe))
        .route(
            "/organizations/{organization_id}/probe",
            get(organization_probe),
        )
        .layer(anubis::guard::layer(pool.clone(), roles));

    let run = Uuid::new_v4();
    let admin_email = format!("guard-admin-{run}@example.com");
    let outsider_email = format!("guard-outsider-{run}@example.com");

    let admin_cookie = register(&router, &admin_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;

    let mut connection = pool.get().await.expect("connection must be available");
    let admin_id: Uuid = users::table
        .filter(users::email.eq(&admin_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the admin must exist");
    let (team_id, organization_id): (Uuid, Uuid) = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(admin_id))),
        )
        .select((teams::id, teams::organization_id))
        .first(&mut connection)
        .await
        .expect("the bootstrapped team must exist");

    let team_probe_path = format!("/teams/{team_id}/probe");
    let org_probe_path = format!("/organizations/{organization_id}/probe");

    // Unauthenticated requests never reach the handler.
    let (status, _headers, _body) = send(&router, "GET", &team_probe_path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The admin holds `manage` on Probe through their bootstrap role.
    let (status, _headers, body) =
        send(&router, "GET", &team_probe_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"], json!("General"));
    let (status, _headers, _body) =
        send(&router, "GET", &org_probe_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::OK);

    // A non-member gets the same 404 as a nonexistent team.
    let (status, _headers, _body) = send(
        &router,
        "GET",
        &team_probe_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let ghost_path = format!("/teams/{}/probe", Uuid::new_v4());
    let (status, _headers, _body) =
        send(&router, "GET", &ghost_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _headers, _body) = send(
        &router,
        "GET",
        "/teams/not-a-uuid/probe",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _headers, _body) = send(
        &router,
        "GET",
        &org_probe_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Invite the outsider as `default`: membership without Probe grants
    // turns the 404 into a 403.
    let invite = json!({ "email": outsider_email, "team_id": team_id, "roles": ["default"] });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let invitation_email = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == outsider_email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation_email
        .text_body
        .split_once("token=")
        .expect("the email must contain a link");
    let token = rest.split_whitespace().next().unwrap_or_default();

    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _headers, _body) = send(
        &router,
        "GET",
        &team_probe_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
