//! Who may create an account, under each registration mode.
//!
//! Every test boots the framework twice over one database: once open, which is
//! how the accounts and invitations a closed deployment still has to serve get
//! made, and once under the mode being proved. That is the shape of the real
//! change too, since a deployment closes registration long after its first
//! accounts exist.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::schema::users;
use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use uuid::Uuid;

use support::{Harness, PASSWORD, TestDatabase, invitation_token, register, send, session_token};

/// How many refusals are timed against one accepted registration.
///
/// The margin is what the assertion rests on: an argon2 computation dominates
/// the cost of creating an account, so refusals that hashed first would each
/// cost about what the accepted one costs and this many could not fit inside
/// it.
const REFUSALS_PER_REGISTRATION: usize = 8;

/// The credentials body every account in this suite registers with.
fn credentials(email: &str) -> Value {
    json!({ "email": email, "password": PASSWORD })
}

/// How many accounts hold `email`, read straight from the table.
async fn accounts_with(harness: &Harness, email: &str) -> i64 {
    let mut connection = harness.pool.get().await.expect("a connection");
    users::table
        .filter(users::email.eq(email))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run")
}

#[tokio::test]
async fn an_invite_only_deployment_refuses_registration_and_still_claims_invitations() {
    let Some(database) = TestDatabase::create("registration_modes_flow").await else {
        return;
    };

    // The deployment before it closed: an admin, and the teammate they are
    // about to invite.
    let open = Harness::boot(&database).await;
    let admin_email = format!("admin-{}@example.com", Uuid::new_v4());
    let admin = register(&open.router, &admin_email).await;
    let member_email = format!("member-{}@example.com", Uuid::new_v4());
    let member = register(&open.router, &member_email).await;
    let team = open.bootstrapped_team(&admin).await;

    let closed = Harness::boot_with_config(
        &database,
        Harness::config_with(&[("ANUBIS_REGISTRATION", "invite_only")]),
    )
    .await;

    // The sign-up screen asks before it renders anything.
    let (status, _headers, body) =
        send(&closed.router, "GET", "/auth/registration", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["open"], json!(false));

    // A stranger is refused, and the refusal says what to do instead.
    let stranger_email = format!("stranger-{}@example.com", Uuid::new_v4());
    let (status, _headers, body) = send(
        &closed.router,
        "POST",
        "/auth/register",
        Some(&credentials(&stranger_email)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert!(
        body["message"]
            .as_str()
            .expect("the error carries a message")
            .contains("invitation"),
        "body: {body}",
    );
    assert_eq!(
        accounts_with(&closed, &stranger_email).await,
        0,
        "a refused registration must leave no account behind",
    );

    // Signing in is not registering: the accounts that already exist are
    // untouched by the mode.
    let (status, _headers, body) = send(
        &closed.router,
        "POST",
        "/auth/login",
        Some(&credentials(&member_email)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // And an invitation is authorization this deployment granted itself, so
    // inviting and claiming both work exactly as they do when open.
    let invitation = json!({ "email": member_email, "team_id": team, "roles": ["default"] });
    let (status, _headers, body) = send(
        &closed.router,
        "POST",
        "/tenancy/invitations",
        Some(&invitation),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let token = invitation_token(&closed.outbox, &member_email);
    let (status, _headers, body) = send(
        &closed.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"]["id"], json!(team));
}

#[tokio::test]
async fn a_domain_allowlist_admits_the_domains_it_names_and_no_others() {
    let Some(database) = TestDatabase::create("registration_modes_flow").await else {
        return;
    };

    let harness = Harness::boot_with_config(
        &database,
        Harness::config_with(&[
            ("ANUBIS_REGISTRATION", "domain_allowlist"),
            ("ANUBIS_REGISTRATION_DOMAINS", "acme.test, Acme.co.uk"),
        ]),
    )
    .await;

    // An allowlist still accepts registrations, so the form belongs on screen
    // and the refusal belongs on submit.
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/registration", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["open"], json!(true));

    let mut admitted = Vec::new();
    for domain in ["acme.test", "ACME.CO.UK"] {
        let email = format!("ada-{}@{domain}", Uuid::new_v4());
        let (status, headers, body) = send(
            &harness.router,
            "POST",
            "/auth/register",
            Some(&credentials(&email)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "for {domain}, body: {body}");
        admitted.push((email.to_lowercase(), session_token(&headers)));
    }

    for domain in [
        // A subdomain is a different domain.
        "mail.acme.test",
        // So is one that merely ends in an admitted domain.
        "notacme.test",
        "example.com",
    ] {
        let email = format!("ada-{}@{domain}", Uuid::new_v4());
        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            "/auth/register",
            Some(&credentials(&email)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "for {domain}, body: {body}");
        assert!(
            body["message"]
                .as_str()
                .expect("the error carries a message")
                .contains("domains"),
            "body: {body}",
        );
        assert_eq!(accounts_with(&harness, &email).await, 0, "for {domain}");
    }

    // Inviting and claiming are untouched here too: the mode decides who may
    // create an account, and both of these people already have one.
    let [(_admin_email, admin), (joiner_email, joiner)] = admitted.as_slice() else {
        panic!("both admitted registrations must have signed in");
    };
    let team = harness.bootstrapped_team(admin).await;
    let invitation = json!({ "email": joiner_email, "team_id": team, "roles": ["default"] });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invitation),
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let token = invitation_token(&harness.outbox, joiner_email);
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(joiner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["team"]["id"], json!(team));
}

/// A refusal costs no argon2, which is the point of checking the mode first.
///
/// Timed against one accepted registration on the same machine rather than
/// against a fixed budget: the accepted one pays for a hash, and if the
/// refusals paid for one each, [`REFUSALS_PER_REGISTRATION`] of them could not
/// finish inside the time the single accepted one took.
#[tokio::test]
async fn a_refused_registration_never_reaches_the_hasher() {
    let Some(database) = TestDatabase::create("registration_modes_flow").await else {
        return;
    };

    let open = Harness::boot(&database).await;
    let closed = Harness::boot_with_config(
        &database,
        Harness::config_with(&[("ANUBIS_REGISTRATION", "invite_only")]),
    )
    .await;

    let accepted_started = std::time::Instant::now();
    register(
        &open.router,
        &format!("timed-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let accepted = accepted_started.elapsed();

    let refused_started = std::time::Instant::now();
    for _attempt in 0..REFUSALS_PER_REGISTRATION {
        let email = format!("refused-{}@example.com", Uuid::new_v4());
        let (status, _headers, body) = send(
            &closed.router,
            "POST",
            "/auth/register",
            Some(&credentials(&email)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    }
    let refused = refused_started.elapsed();

    assert!(
        refused < accepted,
        "{REFUSALS_PER_REGISTRATION} refusals took {refused:?}, one accepted registration took \
         {accepted:?}: the refusals are hashing",
    );
}
