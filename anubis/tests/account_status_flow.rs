//! Disabled accounts and forced password rotation, end to end.
//!
//! Three stories: an account an administrator disabled, an account made to
//! choose a new password, and an administrator reaching for their own account.
//! Each drives the surface the way the browser does, so what is proved is what
//! a person meets.
//!
//! Requires `DATABASE_URL`; without it the tests log a skip and pass. CI
//! always provides one.

mod support;

use anubis::auth::account_status::{ACCOUNT_DISABLED, PASSWORD_CHANGE_REQUIRED};
use anubis::schema::users;
use axum::http::StatusCode;
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::json;
use uuid::Uuid;

use support::{Harness, PASSWORD, TestDatabase, invitation_token, register, send};

const ADMIN_EMAIL: &str = "org-admin@example.com";
const MEMBER_EMAIL: &str = "member@example.com";

/// Disabling is offboarding that takes effect this second: the sessions the
/// account holds are deleted with the flag rather than left to expire, and the
/// credentials that would mint a new one stop working.
#[tokio::test]
async fn a_disabled_account_is_locked_out_of_every_door() {
    let Some(database) = TestDatabase::create("account_status_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, ADMIN_EMAIL).await;
    let organization_id = bootstrapped_organization(&harness, &admin).await;
    let member = join_organization(&harness, &admin, organization_id, MEMBER_EMAIL).await;
    let membership_id = organization_membership(&harness, &admin, organization_id, MEMBER_EMAIL)
        .await
        .expect("the claimed membership must be on the roster");

    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&member)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &account_action(organization_id, membership_id, "disable"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["disabled"], json!(true), "body: {body}");

    // The session the account was holding is gone, not merely refused.
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&member)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");

    // And the credentials that would mint another one are turned away, with
    // the code that tells the browser this is a dead end rather than a detour.
    let credentials = json!({ "email": MEMBER_EMAIL, "password": PASSWORD });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], json!(ACCOUNT_DISABLED), "body: {body}");

    // Re-enabling restores sign-in and nothing else: the revoked sessions stay
    // revoked, so the person signs in again.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &account_action(organization_id, membership_id, "enable"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["disabled"], json!(false), "body: {body}");

    let (status, headers, body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let live_session = support::session_token(&headers);

    // A session that outlived the flag is what a disable racing a sign-in
    // leaves behind. The extractor is the backstop, and it refuses with the
    // same code the front door does.
    disable_in_the_database(&harness, MEMBER_EMAIL).await;
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        "/auth/me",
        None,
        Some(&live_session),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], json!(ACCOUNT_DISABLED), "body: {body}");
}

/// A flagged account may sign in and may change its password. Everything else
/// refuses until it does, and the change is what clears the demand.
#[tokio::test]
async fn a_flagged_account_may_do_exactly_one_thing() {
    let Some(database) = TestDatabase::create("account_status_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, ADMIN_EMAIL).await;
    let organization_id = bootstrapped_organization(&harness, &admin).await;
    let member = join_organization(&harness, &admin, organization_id, MEMBER_EMAIL).await;
    let membership_id = organization_membership(&harness, &admin, organization_id, MEMBER_EMAIL)
        .await
        .expect("the claimed membership must be on the roster");

    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &account_action(organization_id, membership_id, "require-password-change"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["password_change_required"],
        json!(true),
        "body: {body}"
    );

    // Both surfaces refuse, because the extractor they share does.
    for path in ["/auth/me", "/tenancy/memberships"] {
        let (status, _headers, body) =
            send(&harness.router, "GET", path, None, Some(&member)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} body: {body}");
        assert_eq!(
            body["code"],
            json!(PASSWORD_CHANGE_REQUIRED),
            "{path} body: {body}",
        );
    }

    // Signing in again lands in the same place, so the flag cannot be waited
    // out or logged around.
    let (status, headers, body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": MEMBER_EMAIL, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let member = support::session_token(&headers);

    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    // The one route that answers, and the change clears the demand with it.
    let change = json!({ "current_password": PASSWORD, "new_password": "a far better password" });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/auth/change-password",
        Some(&change),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&member)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let mut connection = harness.pool.get().await.expect("connection");
    let still_flagged: bool = users::table
        .filter(users::email.eq(MEMBER_EMAIL))
        .select(users::password_change_required)
        .first(&mut connection)
        .await
        .expect("the member must exist");
    assert!(!still_flagged, "a successful change clears the demand");
}

/// The administrative surface administers other people's accounts, and refuses
/// every one of its three actions aimed at the caller's own.
#[tokio::test]
async fn an_administrator_cannot_act_on_their_own_account() {
    let Some(database) = TestDatabase::create("account_status_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, ADMIN_EMAIL).await;
    let organization_id = bootstrapped_organization(&harness, &admin).await;
    let member = join_organization(&harness, &admin, organization_id, MEMBER_EMAIL).await;

    let own_membership = organization_membership(&harness, &admin, organization_id, ADMIN_EMAIL)
        .await
        .expect("the admin is on their own roster");
    for action in ["disable", "enable", "require-password-change"] {
        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            &account_action(organization_id, own_membership, action),
            None,
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action} body: {body}");
    }

    // The admin can still sign in, which is the point of the refusal.
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // And an ordinary member cannot disable the administrator instead.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &account_action(organization_id, own_membership, "disable"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
}

/// The organization registration bootstrapped for the account behind `cookie`.
async fn bootstrapped_organization(harness: &Harness, cookie: &str) -> Uuid {
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        "/tenancy/memberships",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["organizations"][0]["id"]
        .as_str()
        .expect("registration bootstraps one organization")
        .parse()
        .expect("an organization id is a UUID")
}

/// Registers `email`, invites it into the organization, and claims for it.
///
/// Returns the new member's session token. Organization membership is what the
/// account routes are scoped by, so every story here starts with two people in
/// one organization.
async fn join_organization(
    harness: &Harness,
    admin_cookie: &str,
    organization_id: Uuid,
    email: &str,
) -> String {
    let invite = json!({ "email": email, "organization_id": organization_id, "roles": []});
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let cookie = register(&harness.router, email).await;
    let token = invitation_token(&harness.outbox, email);
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    cookie
}

/// The organization membership the roster lists for `email`.
async fn organization_membership(
    harness: &Harness,
    admin_cookie: &str,
    organization_id: Uuid,
    email: &str,
) -> Option<Uuid> {
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        &format!("/tenancy/organizations/{organization_id}/members"),
        None,
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["members"]
        .as_array()
        .expect("the roster is a list")
        .iter()
        .find(|entry| entry["email"] == json!(email))
        .and_then(|entry| entry["membership_id"].as_str())
        .map(|id| id.parse().expect("a membership id is a UUID"))
}

/// The path of one account action on one member.
fn account_action(organization_id: Uuid, membership_id: Uuid, action: &str) -> String {
    format!("/tenancy/organizations/{organization_id}/members/{membership_id}/{action}")
}

/// Disables an account behind the endpoint's back, leaving its sessions alive.
///
/// What a disable committing in the instant between a session lookup and the
/// request it authorized leaves behind, which is the only way a live session
/// belongs to a disabled account.
async fn disable_in_the_database(harness: &Harness, email: &str) {
    let mut connection = harness.pool.get().await.expect("connection");
    diesel::update(users::table.filter(users::email.eq(email)))
        .set(users::disabled_at.eq(Utc::now()))
        .execute(&mut connection)
        .await
        .expect("the account must exist");
}
