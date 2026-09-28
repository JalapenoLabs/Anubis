//! The platform tier end to end: who operates a deployment, and how they got there.
//!
//! The story is a deployment's first day. Nobody operates it, the environment
//! names an address, the boot appoints that account, and from then on the
//! operator surface exists for exactly one person and is invisible to everyone
//! else. Then the deployment is restarted twice, which is the part worth
//! proving: a seed that is not idempotent is a seed nobody dares leave in a
//! production environment.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::config::AppConfig;
use anubis::guard::PlatformMember;
use anubis::http::ApiError;
use anubis::platform;
use anubis::roles::{Action, OPERATOR_ROLE, RoleSet};
use anubis::schema::{audit_events, users};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::json;
use support::{PASSWORD, TestDatabase, register, send};
use uuid::Uuid;

/// The operator holds `manage` on Console; nobody else is offered the tier at
/// all, which is what makes the guard's `404` the interesting answer.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  admin:
    includes: [default]
    models:
      Console: [manage]
  operator:
    scopes: [platform]
    models:
      Console: [read]
";

/// The password the seeded operator is created with.
const SEEDED_PASSWORD: &str = "a seeded operator password";

/// A route only an operator reaches, and only for the action it names.
async fn console(operator: PlatformMember) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Read, "Console")?;
    Ok(Json(json!({ "operator": operator.user.email })))
}

/// A route an operator reaches but is not permitted to act on.
async fn console_reset(operator: PlatformMember) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Destroy, "Console")?;
    Ok(Json(json!({ "reset": true })))
}

/// The config a deployment naming `email` boots with.
fn config_seeding(email: &str) -> AppConfig {
    let email = email.to_owned();
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "ANUBIS_INITIAL_ADMIN_EMAIL" => Some(email.clone()),
        "ANUBIS_INITIAL_ADMIN_PASSWORD" => Some(SEEDED_PASSWORD.to_owned()),
        _ => None,
    })
    .expect("the seeding config must parse")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_environment_appoints_the_first_operator() {
    let Some(database) = TestDatabase::create("platform_flow").await else {
        return;
    };
    let pool = database.pool().await;

    let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let config = AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("the test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
        )
        .route("/console", get(console))
        .route("/console/reset", get(console_reset))
        .layer(anubis::guard::layer(pool.clone(), roles.clone()));

    // ---- A deployment that names nobody appoints nobody. ----------------
    platform::ensure_initial_admin(&pool, &config, &roles)
        .await
        .expect("a deployment naming no operator must boot");

    let operator_email = format!("ops-{}@example.com", Uuid::new_v4());
    let ordinary_email = format!("member-{}@example.com", Uuid::new_v4());

    // ---- An ordinary account never sees the operator surface. -----------
    let ordinary = register(&router, &ordinary_email).await;
    let (status, _headers, _body) = send(&router, "GET", "/console", None, Some(&ordinary)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the operator surface must not announce itself to the accounts it refuses",
    );

    let (status, _headers, _body) = send(&router, "GET", "/console", None, None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a signed-out caller is asked to sign in, as at every other tier",
    );

    // ---- The first boot that names an address creates that account. -----
    let seeding = config_seeding(&operator_email);
    platform::ensure_initial_admin(&pool, &seeding, &roles)
        .await
        .expect("the seed must appoint the operator");

    let mut connection = pool.get().await.expect("a connection must be available");
    let (operator_id, granted, verified): (
        Uuid,
        Vec<String>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = users::table
        .filter(users::email.eq(&operator_email))
        .select((users::id, users::platform_roles, users::email_verified_at))
        .first(&mut connection)
        .await
        .expect("the seed must have created the account");
    assert_eq!(granted, vec![OPERATOR_ROLE.to_owned()]);
    assert!(
        verified.is_some(),
        "the deployment vouched for the address, so there is no link to click",
    );

    // The account is an ordinary account that also operates: the same
    // bootstrap registration runs gave it a personal organization.
    let (status, headers, body) = send(
        &router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": operator_email, "password": SEEDED_PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let operator = support::session_token(&headers);

    let (status, _headers, body) = send(&router, "GET", "/console", None, Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["operator"], operator_email);

    // Admission is not permission: the tier let them in, the rubric decides
    // what they may do once inside.
    let (status, _headers, _body) =
        send(&router, "GET", "/console/reset", None, Some(&operator)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator without the action is refused, not hidden from",
    );

    // ---- Restarting the deployment changes nothing. ---------------------
    platform::ensure_initial_admin(&pool, &seeding, &roles)
        .await
        .expect("a second boot must be a no-op");

    let held: Vec<String> = users::table
        .find(operator_id)
        .select(users::platform_roles)
        .first(&mut connection)
        .await
        .expect("the operator must still exist");
    assert_eq!(held, vec![OPERATOR_ROLE.to_owned()], "no duplicate grant");

    let grants: i64 = audit_events::table
        .filter(audit_events::subject_id.eq(operator_id))
        .filter(audit_events::action.eq(platform::PLATFORM_ROLES_CHANGED))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the audit log must be readable");
    assert_eq!(grants, 1, "a boot that changed nothing must record nothing");

    // ---- The seed is a grant, never a password reset. -------------------
    // Somebody who already has an account is promoted without their password
    // being replaced by whatever the deployment's environment happens to hold.
    let promoted_email = format!("promoted-{}@example.com", Uuid::new_v4());
    register(&router, &promoted_email).await;

    let promoting = config_seeding(&promoted_email);
    platform::ensure_initial_admin(&pool, &promoting, &roles)
        .await
        .expect("promoting an existing account must succeed");

    let (status, headers, body) = send(
        &router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": promoted_email, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the account keeps the password its owner chose: {body}",
    );
    let promoted = support::session_token(&headers);

    let (status, _headers, _body) = send(&router, "GET", "/console", None, Some(&promoted)).await;
    assert_eq!(status, StatusCode::OK, "the grant took effect");

    let (status, _headers, body) = send(
        &router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": promoted_email, "password": SEEDED_PASSWORD })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the configured password must never become a way into an existing account: {body}",
    );
}

#[tokio::test]
async fn a_deployment_whose_roles_offer_no_operator_refuses_to_seed() {
    let Some(database) = TestDatabase::create("platform_flow_no_role").await else {
        return;
    };
    let pool = database.pool().await;

    // The starter's vocabulary before an application opts into the tier: a
    // file like this one is every application written before it existed.
    let tenancy_only = RoleSet::from_yaml(
        "
roles:
  default:
    models: {}
  admin:
    includes: [default]
    models: {}
",
    )
    .expect("roles must parse");

    let seeding = config_seeding("ops@example.com");
    let error = platform::ensure_initial_admin(&pool, &seeding, &tenancy_only)
        .await
        .expect_err("a grant nothing defines must stop the boot");

    let rendered = error.to_string();
    assert!(rendered.contains(OPERATOR_ROLE), "got: {rendered}");
    assert!(
        rendered.contains("scopes: [platform]"),
        "the message must name the fix, got: {rendered}",
    );
}

#[test]
fn the_seeding_variables_are_validated_at_load() {
    let missing_password = AppConfig::from_lookup(|name| match name {
        "ANUBIS_INITIAL_ADMIN_EMAIL" => Some("ops@example.com".to_owned()),
        _ => None,
    })
    .expect_err("an address with no password must be refused");
    assert!(
        missing_password
            .to_string()
            .contains("ANUBIS_INITIAL_ADMIN_PASSWORD"),
        "got: {missing_password}",
    );

    let missing_email = AppConfig::from_lookup(|name| match name {
        "ANUBIS_INITIAL_ADMIN_PASSWORD" => Some(SEEDED_PASSWORD.to_owned()),
        _ => None,
    })
    .expect_err("a password with nobody to give it to must be refused");
    assert!(
        missing_email
            .to_string()
            .contains("ANUBIS_INITIAL_ADMIN_EMAIL"),
        "got: {missing_email}",
    );

    let short_password = AppConfig::from_lookup(|name| match name {
        "ANUBIS_INITIAL_ADMIN_EMAIL" => Some("ops@example.com".to_owned()),
        "ANUBIS_INITIAL_ADMIN_PASSWORD" => Some("short".to_owned()),
        _ => None,
    })
    .expect_err("a password the seed could never store must be refused");
    let rendered = short_password.to_string();
    assert!(rendered.contains("at least"), "got: {rendered}");
    assert!(
        !rendered.contains("short"),
        "the value is a secret and must never be quoted back, got: {rendered}",
    );

    let config = AppConfig::from_lookup(|name| match name {
        "ANUBIS_INITIAL_ADMIN_EMAIL" => Some("  OPS@Example.COM ".to_owned()),
        "ANUBIS_INITIAL_ADMIN_PASSWORD" => Some(SEEDED_PASSWORD.to_owned()),
        _ => None,
    })
    .expect("a well-formed pair must load");
    let initial_admin = config
        .initial_admin
        .as_ref()
        .expect("the pair must be present");
    assert_eq!(
        initial_admin.email(),
        "ops@example.com",
        "the address is normalized once, where every other address is",
    );
    assert!(
        !format!("{initial_admin:?}").contains(SEEDED_PASSWORD),
        "the password must never reach a log line",
    );
}
