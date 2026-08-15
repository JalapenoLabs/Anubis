//! The way into a deployment nobody can sign into yet.
//!
//! Three stories: an empty deployment seeded from configuration, a populated
//! one left exactly as it was, and the seeded administrator using the account
//! it was handed. The last one is the point of the whole feature, so it is
//! driven the way the browser drives it: sign in, be refused everywhere,
//! change the password, and only then be an administrator.
//!
//! The race two instances booting together run lives in `concurrency_flow`,
//! beside the framework's other races.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::auth::account_status::PASSWORD_CHANGE_REQUIRED;
use anubis::config::AppConfig;
use anubis::schema::{organizations, users};
use anubis::tenancy::seed_first_administrator;
use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};

use support::{Harness, PASSWORD, TestDatabase, register, send, session_token};

/// The administrator an empty deployment in this suite is opened with.
const ADMIN_EMAIL: &str = "founder@example.com";

/// The provisioning password that comes with it, and dies with the first
/// sign-in.
const PROVISIONING_PASSWORD: &str = "provisioned once";

/// The password the seeded account replaces it with.
const CHOSEN_PASSWORD: &str = "a password nobody deployed";

/// The organization a shared deployment in this suite puts everybody in.
const SHARED_ORGANIZATION: &str = "Acme Corporation";

/// A deployment that seeds an administrator and closes the door behind it.
///
/// Invite-only is the shape the seed exists for: no signup can create the
/// first account, so the framework has to.
fn closed_deployment() -> AppConfig {
    Harness::config_with(&[
        ("ANUBIS_REGISTRATION", "invite_only"),
        ("ANUBIS_BOOTSTRAP_ADMIN_EMAIL", ADMIN_EMAIL),
        ("ANUBIS_BOOTSTRAP_ADMIN_PASSWORD", PROVISIONING_PASSWORD),
    ])
}

/// The account row for an address, or `None` when nothing was created.
async fn account(harness: &Harness, email: &str) -> Option<(bool, bool)> {
    let mut connection = harness.pool.get().await.expect("a connection");
    users::table
        .filter(users::email.eq(email))
        .select((
            users::email_verified_at.is_not_null(),
            users::password_change_required,
        ))
        .first(&mut connection)
        .await
        .optional()
        .expect("the account query must run")
}

/// How many accounts exist in the whole database.
async fn account_count(harness: &Harness) -> i64 {
    let mut connection = harness.pool.get().await.expect("a connection");
    users::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run")
}

/// The organizations `GET /tenancy/memberships` lists for one account.
async fn memberships(harness: &Harness, cookie: &str) -> Value {
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        "/tenancy/memberships",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["organizations"].clone()
}

/// Signs in and returns the session cookie, whatever the account owes.
async fn sign_in(harness: &Harness, email: &str, password: &str) -> String {
    let credentials = json!({ "email": email, "password": password });
    let (status, headers, body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers)
}

/// An empty deployment is opened: the account exists, its address is taken as
/// verified, it administers what it landed in, and it already owes the
/// password change that ends the provisioning credential.
#[tokio::test]
async fn an_empty_deployment_is_seeded_with_an_administrator() {
    let Some(database) = TestDatabase::create("first_administrator_flow").await else {
        return;
    };
    let config = closed_deployment();
    let harness = Harness::boot_with_config(&database, config.clone()).await;

    seed_first_administrator(&harness.pool, &config)
        .await
        .expect("an empty deployment must seed");

    let (verified, owes_a_change) = account(&harness, ADMIN_EMAIL)
        .await
        .expect("the configured administrator must exist");
    assert!(
        verified,
        "nothing could have delivered a verification email to a deployment nobody \
         could sign into",
    );
    assert!(
        owes_a_change,
        "a password that came from the environment must not stay a credential",
    );

    // The one thing the account may do is the change, so the tenancy is read
    // after it, the way its owner will.
    let cookie = sign_in(&harness, ADMIN_EMAIL, PROVISIONING_PASSWORD).await;
    let change =
        json!({ "current_password": PROVISIONING_PASSWORD, "new_password": CHOSEN_PASSWORD });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/change-password",
        Some(&change),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let organizations = memberships(&harness, &cookie).await;
    assert_eq!(
        organizations.as_array().map(Vec::len),
        Some(1),
        "one organization: {organizations}",
    );
    assert_eq!(
        organizations[0]["name"],
        json!("founder"),
        "the personal bootstrap, named after the address's local part",
    );
    assert_eq!(
        organizations[0]["roles"],
        json!(["admin"]),
        "an administrator who cannot administer is no way in at all",
    );
    assert_eq!(organizations[0]["teams"][0]["roles"], json!(["admin"]));
}

/// The shared bootstrap is honored, so an internal deployment gets an
/// administrator of the organization everybody else will join.
#[tokio::test]
async fn a_shared_deployment_is_seeded_into_the_organization_it_named() {
    let Some(database) = TestDatabase::create("first_administrator_flow").await else {
        return;
    };
    let config = Harness::config_with(&[
        ("ANUBIS_BOOTSTRAP", "shared"),
        ("ANUBIS_SHARED_ORGANIZATION", SHARED_ORGANIZATION),
        ("ANUBIS_BOOTSTRAP_ADMIN_EMAIL", ADMIN_EMAIL),
        ("ANUBIS_BOOTSTRAP_ADMIN_PASSWORD", PROVISIONING_PASSWORD),
    ]);
    let harness = Harness::boot_with_config(&database, config.clone()).await;

    seed_first_administrator(&harness.pool, &config)
        .await
        .expect("an empty deployment must seed");

    let cookie = sign_in(&harness, ADMIN_EMAIL, PROVISIONING_PASSWORD).await;
    let change =
        json!({ "current_password": PROVISIONING_PASSWORD, "new_password": CHOSEN_PASSWORD });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/change-password",
        Some(&change),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let organizations = memberships(&harness, &cookie).await;
    assert_eq!(
        organizations[0]["name"],
        json!(SHARED_ORGANIZATION),
        "the organization configuration named, not one named after an address",
    );
    assert_eq!(
        organizations[0]["roles"],
        json!(["admin"]),
        "the account everybody else's invitation will come from",
    );

    let mut connection = harness.pool.get().await.expect("a connection");
    let created: i64 = organizations::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run");
    assert_eq!(created, 1, "the one organization the deployment named");
}

/// A deployment with accounts in it is left alone. The variables are inert
/// there, which is a thing to learn from the log rather than from an
/// unexplained new administrator.
#[tokio::test]
async fn a_populated_deployment_is_left_exactly_as_it_was() {
    let Some(database) = TestDatabase::create("first_administrator_flow").await else {
        return;
    };
    // The router boots on the default configuration, because this deployment
    // has to acquire an account before the seed runs. The seed reads the closed
    // one, which is where the variables live.
    let harness = Harness::boot(&database).await;
    let config = closed_deployment();

    let incumbent = register(&harness.router, "incumbent@example.com").await;

    seed_first_administrator(&harness.pool, &config)
        .await
        .expect("a populated deployment must be a no-op, not a failure");

    assert_eq!(
        account(&harness, ADMIN_EMAIL).await,
        None,
        "the configured administrator was not created",
    );
    assert_eq!(account_count(&harness).await, 1, "and nobody else was");

    // The account that was already there is untouched, in the one respect the
    // seed would have changed: it owes nothing.
    let (_verified, owes_a_change) = account(&harness, "incumbent@example.com")
        .await
        .expect("the incumbent must still exist");
    assert!(!owes_a_change, "an existing account is not re-provisioned");

    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&incumbent)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// The seeded credential is a provisioning credential and behaves like one:
/// it opens exactly one door, the change that replaces it, and the account is
/// an ordinary administrator afterwards.
#[tokio::test]
async fn the_seeded_administrator_must_replace_its_password_before_anything_else() {
    let Some(database) = TestDatabase::create("first_administrator_flow").await else {
        return;
    };
    let config = closed_deployment();
    let harness = Harness::boot_with_config(&database, config.clone()).await;

    seed_first_administrator(&harness.pool, &config)
        .await
        .expect("an empty deployment must seed");

    // Registration is closed, which is the deployment the seed exists for:
    // without it there would be no way to make this account at all.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/register",
        Some(&json!({ "email": "stranger@example.com", "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    let cookie = sign_in(&harness, ADMIN_EMAIL, PROVISIONING_PASSWORD).await;

    for path in ["/auth/me", "/tenancy/memberships"] {
        let (status, _headers, body) =
            send(&harness.router, "GET", path, None, Some(&cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} body: {body}");
        assert_eq!(
            body["code"],
            json!(PASSWORD_CHANGE_REQUIRED),
            "{path} body: {body}",
        );
    }

    let change =
        json!({ "current_password": PROVISIONING_PASSWORD, "new_password": CHOSEN_PASSWORD });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/change-password",
        Some(&change),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // An ordinary administrator from here: the door the demand held shut is
    // open, and the credential that was written into the environment no longer
    // opens anything.
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let credentials = json!({ "email": ADMIN_EMAIL, "password": PROVISIONING_PASSWORD });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");

    // And the invitation the whole seed exists to make is one it can send.
    let team_id = harness.bootstrapped_team(&cookie).await;
    let invite =
        json!({ "email": "colleague@example.com", "team_id": team_id, "roles": ["editor"] });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
}
