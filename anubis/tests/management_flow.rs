//! Tenancy management against a real Postgres database.
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

use anubis::db::DbPool;
use anubis::schema::{organization_memberships, organizations, team_memberships, teams, users};

const PASSWORD: &str = "correct horse battery staple";

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
    let credentials = json!({ "email": email, "password": PASSWORD });
    let (status, headers, body) =
        send(router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// Composes the routers exactly as a starter application does.
async fn application(database_url: &str) -> (Router, DbPool, anubis::mail::TestOutbox) {
    anubis::db::run_pending_migrations(database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let roles = anubis::roles::RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let (mailer, outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config, &rate_limit),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles, None, &config, &rate_limit),
        );

    (router, pool, outbox)
}

/// Sends an invitation and returns its id and the emailed token.
async fn invite(
    router: &Router,
    outbox: &anubis::mail::TestOutbox,
    admin_cookie: &str,
    body: &Value,
    email: &str,
) -> (Uuid, String) {
    let (status, _headers, response) = send(
        router,
        "POST",
        "/tenancy/invitations",
        Some(body),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {response}");

    let invitation_id = uuid(&response["invitation"]["id"]);
    let sent = outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email)
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = sent
        .text_body
        .split_once("token=")
        .expect("the invitation email must contain a link");
    let token = rest
        .split_whitespace()
        .next()
        .expect("the token must end at whitespace")
        .to_owned();

    (invitation_id, token)
}

async fn claim(router: &Router, cookie: &str, token: &str) {
    let (status, _headers, body) = send(
        router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

async fn roster(router: &Router, team_id: Uuid, cookie: &str) -> Value {
    let path = format!("/tenancy/teams/{team_id}/members");
    let (status, _headers, body) = send(router, "GET", &path, None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body
}

async fn organization_roster(router: &Router, organization_id: Uuid, cookie: &str) -> Value {
    let path = format!("/tenancy/organizations/{organization_id}/members");
    let (status, _headers, body) = send(router, "GET", &path, None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body
}

fn uuid(value: &Value) -> Uuid {
    value
        .as_str()
        .expect("the field must be a string")
        .parse()
        .expect("the field must be a uuid")
}

/// The roster entry for an email address.
fn entry<'roster>(roster: &'roster Value, email: &str) -> &'roster Value {
    roster["members"]
        .as_array()
        .expect("members must be an array")
        .iter()
        .find(|member| member["email"] == json!(email))
        .unwrap_or_else(|| panic!("{email} must be on the roster: {roster}"))
}

fn member_count(roster: &Value) -> usize {
    roster["members"]
        .as_array()
        .expect("members must be an array")
        .len()
}

/// Whether an email is anywhere on the roster.
fn lists(roster: &Value, email: &str) -> bool {
    roster["members"]
        .as_array()
        .expect("members must be an array")
        .iter()
        .any(|member| member["email"] == json!(email))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn tenants_are_created_administered_and_dissolved() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping management_flow test: DATABASE_URL is not set");
        return;
    };
    let (router, pool, outbox) = application(&database_url).await;
    let mut connection = pool.get().await.expect("connection must be available");

    let run = Uuid::new_v4();
    let founder_email = format!("founder-{run}@example.com");
    let teammate_email = format!("teammate-{run}@example.com");
    let contractor_email = format!("contractor-{run}@example.com");
    let biller_email = format!("biller-{run}@example.com");
    let advisor_email = format!("advisor-{run}@example.com");
    let outsider_email = format!("outsider-{run}@example.com");
    let lurker_email = format!("lurker-{run}@example.com");

    let founder_cookie = register(&router, &founder_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;

    let founder_id: Uuid = users::table
        .filter(users::email.eq(&founder_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the founder must exist");
    let personal_organization_id: Uuid = organization_memberships::table
        .filter(organization_memberships::user_id.eq(founder_id))
        .select(organization_memberships::organization_id)
        .first(&mut connection)
        .await
        .expect("the bootstrapped organization must exist");

    // ------------------------------------------------------------------
    // Creating an organization: the creator administers it and its team.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/organizations",
        Some(&json!({ "name": "  Acme  " })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["organization"]["name"], json!("Acme"), "names trim");
    assert_eq!(body["team"]["name"], json!("General"));
    let acme_id = uuid(&body["organization"]["id"]);

    let organization_roles: Vec<String> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(acme_id))
        .filter(organization_memberships::user_id.eq(founder_id))
        .select(organization_memberships::roles)
        .first(&mut connection)
        .await
        .expect("the creator's organization membership must exist");
    assert_eq!(organization_roles, vec!["admin".to_owned()]);

    let (status, _headers, _body) = send(
        &router,
        "POST",
        "/tenancy/organizations",
        Some(&json!({ "name": "   " })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a name is required");

    // ------------------------------------------------------------------
    // Renaming, and creating a second team inside the organization.
    // ------------------------------------------------------------------
    let organization_path = format!("/tenancy/organizations/{acme_id}");
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &organization_path,
        Some(&json!({ "name": "Acme Inc" })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["organization"]["name"], json!("Acme Inc"));

    let teams_path = format!("/tenancy/organizations/{acme_id}/teams");
    let (status, _headers, body) = send(
        &router,
        "POST",
        &teams_path,
        Some(&json!({ "name": "Platform" })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let platform_id = uuid(&body["team"]["id"]);

    let team_path = format!("/tenancy/teams/{platform_id}");
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &team_path,
        Some(&json!({ "name": "Platform Crew" })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"]["name"], json!("Platform Crew"));

    let listed = roster(&router, platform_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 1, "the creator is the only member");
    let founder_membership = uuid(&entry(&listed, &founder_email)["membership_id"]);

    // ------------------------------------------------------------------
    // Roles: an admin sets them, a non-admin cannot, unknown keys are out.
    // ------------------------------------------------------------------
    let teammate_cookie = register(&router, &teammate_email).await;
    let (claimed_invitation, token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": teammate_email, "team_id": platform_id, "roles": ["editor"] }),
        &teammate_email,
    )
    .await;
    claim(&router, &teammate_cookie, &token).await;

    let listed = roster(&router, platform_id, &founder_cookie).await;
    let teammate_membership = uuid(&entry(&listed, &teammate_email)["membership_id"]);

    let teammate_path = format!("/tenancy/teams/{platform_id}/members/{teammate_membership}");
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &teammate_path,
        Some(&json!({ "roles": ["editor", "billing"] })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["roles"], json!(["editor", "billing"]));

    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &teammate_path,
        Some(&json!({ "roles": ["emperor"] })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown roles are refused");

    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &team_path,
        Some(&json!({ "name": "Renamed by an editor" })),
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a member is not an admin");

    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &teammate_path,
        Some(&json!({ "roles": ["admin"] })),
        Some(&teammate_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no self-promotion");

    // A non-member cannot tell the team from one that does not exist.
    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &team_path,
        Some(&json!({ "name": "Mine now" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &organization_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // ------------------------------------------------------------------
    // The last admin cannot be demoted, and cannot walk out.
    // ------------------------------------------------------------------
    let founder_path = format!("/tenancy/teams/{platform_id}/members/{founder_membership}");
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &founder_path,
        Some(&json!({ "roles": ["editor"] })),
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    let leave_path = format!("/tenancy/teams/{platform_id}/leave");
    let (status, _headers, _body) =
        send(&router, "POST", &leave_path, None, Some(&founder_cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // ------------------------------------------------------------------
    // Invitations: pending ones are revocable, claimed ones are gone.
    // ------------------------------------------------------------------
    let (pending_invitation, _token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": lurker_email, "team_id": platform_id }),
        &lurker_email,
    )
    .await;

    let listed = roster(&router, platform_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 3, "the invitee holds a place");
    let lurker = entry(&listed, &lurker_email);
    assert_eq!(lurker["pending"], json!(true));
    assert_eq!(uuid(&lurker["invitation_id"]), pending_invitation);

    let revoke_path = format!("/tenancy/teams/{platform_id}/invitations/{pending_invitation}");
    let (status, _headers, _body) =
        send(&router, "DELETE", &revoke_path, None, Some(&founder_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listed = roster(&router, platform_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 2, "the held place is released");

    let (status, _headers, _body) =
        send(&router, "DELETE", &revoke_path, None, Some(&founder_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "revoking twice is a 404");

    let claimed_path = format!("/tenancy/teams/{platform_id}/invitations/{claimed_invitation}");
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &claimed_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a claim cannot be undone");

    // ------------------------------------------------------------------
    // Leaving and removing.
    // ------------------------------------------------------------------
    let contractor_cookie = register(&router, &contractor_email).await;
    let (_invitation, token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": contractor_email, "team_id": platform_id }),
        &contractor_email,
    )
    .await;
    claim(&router, &contractor_cookie, &token).await;

    let (status, _headers, _body) =
        send(&router, "POST", &leave_path, None, Some(&contractor_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _body) = send(
        &router,
        "GET",
        &format!("/tenancy/teams/{platform_id}/members"),
        None,
        Some(&contractor_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the door closes behind them");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &founder_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "leaving has its own route");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &teammate_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listed = roster(&router, platform_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 1);

    // ------------------------------------------------------------------
    // Organization roles: a member without the admin key is refused.
    // ------------------------------------------------------------------
    let biller_cookie = register(&router, &biller_email).await;
    let (_invitation, token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": biller_email, "organization_id": acme_id, "roles": ["billing"] }),
        &biller_email,
    )
    .await;
    claim(&router, &biller_cookie, &token).await;

    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &organization_path,
        Some(&json!({ "name": "Biller Inc" })),
        Some(&biller_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _headers, _body) = send(
        &router,
        "POST",
        &teams_path,
        Some(&json!({ "name": "Shadow" })),
        Some(&biller_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // ------------------------------------------------------------------
    // The organization roster: its members, and the invitations still out.
    // ------------------------------------------------------------------
    let listed = organization_roster(&router, acme_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 2, "the founder and the biller");
    assert_eq!(entry(&listed, &biller_email)["pending"], json!(false));
    let founder_organization_membership = uuid(&entry(&listed, &founder_email)["membership_id"]);

    let advisor_cookie = register(&router, &advisor_email).await;
    let (advisor_invitation, token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": advisor_email, "organization_id": acme_id, "roles": ["billing"] }),
        &advisor_email,
    )
    .await;

    // A team invitation holds a place on its team's roster, not on this one.
    let (_invitation, _token) = invite(
        &router,
        &outbox,
        &founder_cookie,
        &json!({ "email": lurker_email, "team_id": platform_id }),
        &lurker_email,
    )
    .await;

    let listed = organization_roster(&router, acme_id, &founder_cookie).await;
    assert!(
        !lists(&listed, &lurker_email),
        "that invitation is the team's"
    );
    let advisor = entry(&listed, &advisor_email);
    assert_eq!(advisor["pending"], json!(true));
    assert_eq!(
        advisor["membership_id"],
        Value::Null,
        "an organization invitation creates no membership until it is claimed",
    );
    assert_eq!(uuid(&advisor["invitation_id"]), advisor_invitation);

    // Any member sees the roster; a non-member cannot tell it exists.
    let listed = organization_roster(&router, acme_id, &biller_cookie).await;
    assert_eq!(member_count(&listed), 3);
    let (status, _headers, _body) = send(
        &router,
        "GET",
        &format!("/tenancy/organizations/{acme_id}/members"),
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    claim(&router, &advisor_cookie, &token).await;
    let listed = organization_roster(&router, acme_id, &founder_cookie).await;
    let advisor = entry(&listed, &advisor_email);
    assert_eq!(advisor["pending"], json!(false), "the claim seats them");
    let advisor_membership = uuid(&advisor["membership_id"]);

    // ------------------------------------------------------------------
    // Leaving and removing at the organization level.
    // ------------------------------------------------------------------
    let advisor_path = format!("/tenancy/organizations/{acme_id}/members/{advisor_membership}");
    let founder_organization_path =
        format!("/tenancy/organizations/{acme_id}/members/{founder_organization_membership}");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &founder_organization_path,
        None,
        Some(&advisor_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a biller is not an admin");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &founder_organization_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "leaving has its own route");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &advisor_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let organization_leave_path = format!("/tenancy/organizations/{acme_id}/leave");
    let (status, _headers, _body) = send(
        &router,
        "POST",
        &organization_leave_path,
        None,
        Some(&biller_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listed = organization_roster(&router, acme_id, &founder_cookie).await;
    assert_eq!(member_count(&listed), 1, "the founder is what is left");

    let (status, _headers, body) = send(
        &router,
        "POST",
        &organization_leave_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    // ------------------------------------------------------------------
    // Dissolving: a team goes with its organization, or on its own.
    // ------------------------------------------------------------------
    let foreign_path =
        format!("/tenancy/organizations/{personal_organization_id}/teams/{platform_id}");
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &foreign_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a team belongs to one organization",
    );

    let delete_team_path = format!("/tenancy/organizations/{acme_id}/teams/{platform_id}");
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &delete_team_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &organization_path,
        None,
        Some(&founder_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let surviving: i64 = organizations::table
        .filter(organizations::id.eq(acme_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(surviving, 0, "the organization is gone");

    let surviving_teams: i64 = teams::table
        .filter(teams::organization_id.eq(acme_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(surviving_teams, 0, "its teams went with it");

    let surviving_memberships: i64 = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(acme_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(surviving_memberships, 0, "and so did its memberships");

    // The founder still has the organization they signed up with.
    let personal: i64 = organizations::table
        .filter(organizations::id.eq(personal_organization_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(personal, 1);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn account_deletion_settles_the_organizations_left_behind() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping management_flow test: DATABASE_URL is not set");
        return;
    };
    let (router, pool, outbox) = application(&database_url).await;
    let mut connection = pool.get().await.expect("connection must be available");

    let run = Uuid::new_v4();
    let owner_email = format!("owner-{run}@example.com");
    let heir_email = format!("heir-{run}@example.com");
    let solo_email = format!("solo-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let heir_cookie = register(&router, &heir_email).await;
    let solo_cookie = register(&router, &solo_email).await;

    let owner_id: Uuid = users::table
        .filter(users::email.eq(&owner_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the owner must exist");
    let heir_id: Uuid = users::table
        .filter(users::email.eq(&heir_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the heir must exist");
    let solo_id: Uuid = users::table
        .filter(users::email.eq(&solo_email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the solo user must exist");

    let (shared_organization, shared_team): (Uuid, Uuid) = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(owner_id))),
        )
        .select((teams::organization_id, teams::id))
        .first(&mut connection)
        .await
        .expect("the owner's bootstrapped team must exist");
    let solo_organization: Uuid = organization_memberships::table
        .filter(organization_memberships::user_id.eq(solo_id))
        .select(organization_memberships::organization_id)
        .first(&mut connection)
        .await
        .expect("the solo user's organization must exist");

    // The heir joins the owner's team, and holds no organization membership.
    let (_invitation, token) = invite(
        &router,
        &outbox,
        &owner_cookie,
        &json!({ "email": heir_email, "team_id": shared_team, "roles": ["editor"] }),
        &heir_email,
    )
    .await;
    claim(&router, &heir_cookie, &token).await;

    // ------------------------------------------------------------------
    // A sole member's organization goes with them.
    // ------------------------------------------------------------------
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/account",
        Some(&json!({ "password": PASSWORD })),
        Some(&solo_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let deserted: i64 = organizations::table
        .filter(organizations::id.eq(solo_organization))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(deserted, 0, "nobody was left to reach it");

    // ------------------------------------------------------------------
    // A shared organization survives, and keeps an admin.
    // ------------------------------------------------------------------
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/account",
        Some(&json!({ "password": PASSWORD })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let survivors: i64 = organizations::table
        .filter(organizations::id.eq(shared_organization))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(survivors, 1, "the heir still works here");

    let team_roles: Vec<String> = team_memberships::table
        .filter(team_memberships::team_id.eq(shared_team))
        .filter(team_memberships::user_id.eq(heir_id))
        .select(team_memberships::roles)
        .first(&mut connection)
        .await
        .expect("the heir's team membership must survive");
    assert!(
        team_roles.contains(&"admin".to_owned()),
        "the last admin's departure promotes the heir: {team_roles:?}",
    );

    let organization_roles: Vec<String> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(shared_organization))
        .filter(organization_memberships::user_id.eq(heir_id))
        .select(organization_memberships::roles)
        .first(&mut connection)
        .await
        .expect("the heir must inherit the organization membership");
    assert_eq!(organization_roles, vec!["admin".to_owned()]);
}
