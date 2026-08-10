//! Invitation lifecycle against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use anubis::schema::{invitations, organization_memberships, team_memberships, teams, users};

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

fn invitation_token(email_body: &str) -> String {
    let (_before, rest) = email_body
        .split_once("token=")
        .expect("the invitation email must contain a link");
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
async fn invitations_are_sent_claimed_and_guarded() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping invitations_flow test: DATABASE_URL is not set");
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
            anubis::tenancy::router(pool.clone(), mailer, roles, &config),
        );

    let run = Uuid::new_v4();
    let admin_email = format!("admin-{run}@example.com");
    let teammate_email = format!("teammate-{run}@example.com");
    let biller_email = format!("biller-{run}@example.com");

    let admin_cookie = register(&router, &admin_email).await;

    let mut connection = pool.get().await.expect("connection must be available");

    // The admin's bootstrapped org and team.
    let admin_id: Uuid = users::table
        .filter(users::email.eq(&admin_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the admin must exist");
    let (organization_id, team_id): (Uuid, Uuid) = teams::table
        .inner_join(anubis::schema::organizations::table)
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(admin_id))),
        )
        .select((teams::organization_id, teams::id))
        .first(&mut connection)
        .await
        .expect("the bootstrapped team must exist");

    // ------------------------------------------------------------------
    // Team invitation: membership exists before the claim and survives it.
    // ------------------------------------------------------------------
    let invite = json!({ "email": teammate_email, "team_id": team_id, "roles": ["editor"] });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let unclaimed_id: Uuid = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_null())
        .select(team_memberships::id)
        .first(&mut connection)
        .await
        .expect("the unclaimed membership must exist");

    let invitation_email = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == teammate_email)
        .expect("the invitation email must be in the outbox");
    let team_token = invitation_token(&invitation_email.text_body);

    let teammate_cookie = register(&router, &teammate_email).await;
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": team_token })),
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"]["name"], json!("General"), "body: {body}");

    let teammate_id: Uuid = users::table
        .filter(users::email.eq(&teammate_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the teammate must exist");
    let (claimed_user, claimed_roles): (Option<Uuid>, Vec<String>) = team_memberships::table
        .filter(team_memberships::id.eq(unclaimed_id))
        .select((team_memberships::user_id, team_memberships::roles))
        .first(&mut connection)
        .await
        .expect("the membership must survive the claim under the same id");
    assert_eq!(claimed_user, Some(teammate_id));
    assert_eq!(claimed_roles, vec!["editor".to_owned()]);

    // The invitation is single-use.
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": team_token })),
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ------------------------------------------------------------------
    // Authorization: an editor cannot invite; garbage input is rejected.
    // ------------------------------------------------------------------
    let sneaky = json!({ "email": "x@example.com", "team_id": team_id });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&sneaky),
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let unknown_role =
        json!({ "email": "x@example.com", "team_id": team_id, "roles": ["emperor"] });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&unknown_role),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let both_targets =
        json!({ "email": "x@example.com", "team_id": team_id, "organization_id": organization_id });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&both_targets),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ------------------------------------------------------------------
    // Organization invitation: claimed into an org membership.
    // ------------------------------------------------------------------
    let org_invite =
        json!({ "email": biller_email, "organization_id": organization_id, "roles": ["billing"] });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&org_invite),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let org_email = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == biller_email)
        .expect("the org invitation email must be in the outbox");
    let org_token = invitation_token(&org_email.text_body);

    let biller_cookie = register(&router, &biller_email).await;
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": org_token })),
        Some(&biller_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"], Value::Null, "org invites join no team");

    let biller_id: Uuid = users::table
        .filter(users::email.eq(&biller_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the biller must exist");
    let biller_roles: Vec<String> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::user_id.eq(biller_id))
        .select(organization_memberships::roles)
        .first(&mut connection)
        .await
        .expect("the org membership must exist");
    assert_eq!(biller_roles, vec!["billing".to_owned()]);

    // No invitations linger for this run's emails.
    let pending: i64 = invitations::table
        .filter(invitations::email.eq_any([&teammate_email, &biller_email]))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(pending, 0);

    // ------------------------------------------------------------------
    // Memberships overview: the teammate sees their personal org plus the
    // admin's org (through the team membership, with no org-level roles).
    // ------------------------------------------------------------------
    let teammate_cookie = {
        let credentials =
            json!({ "email": teammate_email, "password": "correct horse battery staple" });
        let (status, headers, body) =
            send(&router, "POST", "/auth/login", Some(&credentials), None).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        session_token(&headers)
    };

    let (status, _headers, body) = send(
        &router,
        "GET",
        "/tenancy/memberships",
        None,
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let listed = body["organizations"]
        .as_array()
        .expect("organizations must be an array");
    assert_eq!(listed.len(), 2, "body: {body}");
    let admin_org = listed
        .iter()
        .find(|org| org["id"] == json!(organization_id.to_string()))
        .expect("the admin's org must be listed");
    assert_eq!(admin_org["roles"], json!([]), "no org-level roles");
    assert_eq!(admin_org["teams"][0]["name"], json!("General"));
    assert_eq!(admin_org["teams"][0]["roles"], json!(["editor"]));

    // ------------------------------------------------------------------
    // Roster: members and pending invitations; non-members get 404.
    // ------------------------------------------------------------------
    let lurker_email = format!("lurker-{run}@example.com");
    let invite = json!({ "email": lurker_email, "team_id": team_id });
    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let roster_path = format!("/tenancy/teams/{team_id}/members");
    let (status, _headers, body) =
        send(&router, "GET", &roster_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let members = body["members"]
        .as_array()
        .expect("members must be an array");
    assert_eq!(members.len(), 3, "admin, teammate, pending: {body}");
    let pending_member = members
        .iter()
        .find(|entry| entry["pending"] == json!(true))
        .expect("the pending invitation must be listed");
    assert_eq!(pending_member["email"], json!(lurker_email));
    assert_eq!(pending_member["roles"], json!(["default"]));

    // The biller holds an org membership but no team membership: 404.
    let biller_cookie = {
        let credentials =
            json!({ "email": biller_email, "password": "correct horse battery staple" });
        let (_status, headers, _body) =
            send(&router, "POST", "/auth/login", Some(&credentials), None).await;
        session_token(&headers)
    };
    let (status, _headers, _body) =
        send(&router, "GET", &roster_path, None, Some(&biller_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
