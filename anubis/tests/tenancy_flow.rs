//! Tenancy bootstrap against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::json;
use tower::ServiceExt;

use anubis::schema::{organization_memberships, organizations, team_memberships, teams, users};
use anubis::tenancy::{ADMIN_ROLE, Organization, OrganizationMembership, Team, TeamMembership};

#[tokio::test]
async fn registration_bootstraps_a_personal_organization() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping tenancy_flow test: DATABASE_URL is not set");
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
    let router = anubis::auth::router(pool.clone(), mailer, &config);

    let local_part = format!("tenant-{}", uuid::Uuid::new_v4());
    let email = format!("{local_part}@example.com");

    let request = Request::builder()
        .method("POST")
        .uri("/register")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "email": email,
                "password": "correct horse battery staple",
            }))
            .expect("body must serialize"),
        ))
        .expect("request must build");

    let response = router
        .oneshot(request)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::CREATED);

    // Walk the bootstrap chain straight from the database.
    let mut connection = pool.get().await.expect("connection must be available");

    let user_id: uuid::Uuid = users::table
        .filter(users::email.eq(&email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the user must exist");

    let org_membership: OrganizationMembership = organization_memberships::table
        .filter(organization_memberships::user_id.eq(user_id))
        .select(OrganizationMembership::as_select())
        .first(&mut connection)
        .await
        .expect("an organization membership must exist");
    assert_eq!(org_membership.roles, vec![ADMIN_ROLE.to_owned()]);

    let organization: Organization = organizations::table
        .find(org_membership.organization_id)
        .select(Organization::as_select())
        .first(&mut connection)
        .await
        .expect("the organization must exist");
    assert_eq!(organization.name, local_part);

    let team: Team = teams::table
        .filter(teams::organization_id.eq(organization.id))
        .select(Team::as_select())
        .first(&mut connection)
        .await
        .expect("the default team must exist");
    assert_eq!(team.name, "General");

    let team_membership: TeamMembership = team_memberships::table
        .filter(team_memberships::team_id.eq(team.id))
        .select(TeamMembership::as_select())
        .first(&mut connection)
        .await
        .expect("a team membership must exist");
    assert_eq!(team_membership.user_id, Some(user_id));
    assert_eq!(team_membership.roles, vec![ADMIN_ROLE.to_owned()]);
}
