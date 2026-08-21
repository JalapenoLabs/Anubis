//! The audit log end to end: what gets recorded, and who may read it.
//!
//! One team, two people, and the acts a framework performs on their behalf. The
//! story runs through the real routers, so every row this suite reads was
//! written by the handler that would have written it in production, on the same
//! connection, inside the same transaction.
//!
//! Three properties matter, and each has its own act below: framework surfaces
//! record themselves without being asked, credentials never reach the change
//! set, and the listing is admin-only and paged.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::audit::{self, Changes, Context, Event};
use anubis::schema::audit_events;
use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use support::{Harness, PASSWORD, TestDatabase, invitation_token, register, send};
use uuid::Uuid;

/// The suite's fixed cast, so a story reads as a story.
struct Cast {
    harness: Harness,
    /// The team admin, who acts and who reads the log.
    owner: String,
    owner_email: String,
    /// The team the acts happen in.
    team: Uuid,
}

async fn open(narrative: &str) -> Option<(TestDatabase, Cast)> {
    let database = TestDatabase::create(narrative).await?;
    let harness = Harness::boot(&database).await;

    let owner_email = format!("owner-{}@example.com", Uuid::new_v4());
    let owner = register(&harness.router, &owner_email).await;
    let team = harness.bootstrapped_team(&owner).await;

    Some((
        database,
        Cast {
            harness,
            owner,
            owner_email,
            team,
        },
    ))
}

/// Reads the team's log as the owner, who is its admin.
async fn team_log(cast: &Cast, query: &str) -> (StatusCode, Value) {
    let path = format!("/account/teams/{}/audit-events{query}", cast.team);
    let (status, _headers, body) =
        send(&cast.harness.router, "GET", &path, None, Some(&cast.owner)).await;
    (status, body)
}

/// The actions in a listing, newest first, which is the order it answers in.
fn actions(body: &Value) -> Vec<String> {
    body["audit_events"]
        .as_array()
        .expect("a listing answers with an array")
        .iter()
        .map(|event| {
            event["action"]
                .as_str()
                .expect("every event names an action")
                .to_owned()
        })
        .collect()
}

fn find<'body>(body: &'body Value, action: &str) -> &'body Value {
    body["audit_events"]
        .as_array()
        .expect("a listing answers with an array")
        .iter()
        .find(|event| event["action"] == action)
        .unwrap_or_else(|| panic!("the log must hold a {action} event: {body}"))
}

#[tokio::test]
async fn tenancy_records_itself_and_the_team_admin_can_read_it() {
    let Some((_database, cast)) = open("audit_flow::tenancy").await else {
        return;
    };

    // A rename, which is the simplest act with a before and an after.
    let (status, body) = {
        let path = format!("/tenancy/teams/{}", cast.team);
        let (status, _headers, body) = send(
            &cast.harness.router,
            "PATCH",
            &path,
            Some(&json!({ "name": "Rocketry" })),
            Some(&cast.owner),
        )
        .await;
        (status, body)
    };
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // An invitation, and somebody claiming it.
    let guest_email = format!("guest-{}@example.com", Uuid::new_v4());
    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&json!({
            "email": guest_email,
            "team_id": cast.team,
            "roles": [ "editor" ],
        })),
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let guest = register(&cast.harness.router, &guest_email).await;
    let token = invitation_token(&cast.harness.outbox, &guest_email);
    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&guest),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, log) = team_log(&cast, "").await;
    assert_eq!(status, StatusCode::OK, "body: {log}");

    let recorded = actions(&log);
    for expected in [
        audit::TEAM_RENAMED,
        audit::INVITATION_CREATED,
        audit::INVITATION_CLAIMED,
        audit::MEMBER_ADDED,
    ] {
        assert!(
            recorded.iter().any(|action| action == expected),
            "expected {expected} in {recorded:?}",
        );
    }

    // Newest first: the claim happened last, so it leads.
    assert!(
        recorded[0] == audit::MEMBER_ADDED || recorded[0] == audit::INVITATION_CLAIMED,
        "the newest act must lead: {recorded:?}",
    );

    let renamed = find(&log, audit::TEAM_RENAMED);
    assert_eq!(renamed["subject_type"], "Team");
    assert_eq!(renamed["subject_label"], "Rocketry");
    assert_eq!(renamed["changes"]["name"]["new"], "Rocketry");
    assert_eq!(renamed["changes"]["name"]["old"], "General");
    // The actor is copied onto the row, not joined at read time.
    assert_eq!(renamed["actor_name"], cast.owner_email);

    let invited = find(&log, audit::INVITATION_CREATED);
    assert_eq!(invited["subject_type"], "Invitation");
    assert_eq!(invited["subject_label"], guest_email);
    assert_eq!(invited["changes"]["roles"]["new"], json!(["editor"]));
}

#[tokio::test]
async fn a_role_change_and_a_removal_name_the_member_and_the_roles() {
    let Some((_database, cast)) = open("audit_flow::membership").await else {
        return;
    };

    let guest_email = format!("guest-{}@example.com", Uuid::new_v4());
    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&json!({
            "email": guest_email,
            "team_id": cast.team,
            "roles": [ "default" ],
        })),
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // The roster names the pending membership, which is what both acts target.
    let path = format!("/tenancy/teams/{}/members", cast.team);
    let (status, _headers, roster) =
        send(&cast.harness.router, "GET", &path, None, Some(&cast.owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {roster}");
    let membership_id = roster["members"]
        .as_array()
        .expect("the roster answers with an array")
        .iter()
        .find(|member| member["email"] == guest_email.as_str())
        .and_then(|member| member["membership_id"].as_str())
        .expect("the invited member must be on the roster")
        .to_owned();

    let path = format!("/tenancy/teams/{}/members/{membership_id}", cast.team);
    let (status, _headers, body) = send(
        &cast.harness.router,
        "PATCH",
        &path,
        Some(&json!({ "roles": [ "editor" ] })),
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, body) = send(
        &cast.harness.router,
        "DELETE",
        &path,
        None,
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let (status, log) = team_log(&cast, "").await;
    assert_eq!(status, StatusCode::OK, "body: {log}");

    let changed = find(&log, audit::MEMBER_ROLE_CHANGED);
    assert_eq!(changed["subject_type"], "TeamMembership");
    // A pending member reads as the address their invitation went to.
    assert_eq!(changed["subject_label"], guest_email);
    assert_eq!(changed["changes"]["roles"]["old"], json!(["default"]));
    assert_eq!(changed["changes"]["roles"]["new"], json!(["editor"]));

    let removed = find(&log, audit::MEMBER_REMOVED);
    // The label was read before the delete, so it survives the row it named.
    assert_eq!(removed["subject_label"], guest_email);
    assert_eq!(removed["changes"]["roles"]["new"], json!([]));
}

#[tokio::test]
async fn credential_changes_are_recorded_without_the_credentials() {
    let Some((_database, cast)) = open("audit_flow::credentials").await else {
        return;
    };

    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/auth/change-password",
        Some(&json!({
            "current_password": PASSWORD,
            "new_password": "a far better password than the last",
        })),
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // An account event belongs to the person, not to a tenant, so the team's
    // log does not hold it.
    let (status, team) = team_log(&cast, "").await;
    assert_eq!(status, StatusCode::OK, "body: {team}");
    assert!(
        !actions(&team)
            .iter()
            .any(|action| action == audit::PASSWORD_CHANGED),
        "a password change is not a team act: {team}",
    );

    let (status, _headers, own) = send(
        &cast.harness.router,
        "GET",
        "/account/audit-events",
        None,
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {own}");

    let changed = find(&own, audit::PASSWORD_CHANGED);
    assert_eq!(changed["subject_type"], "User");
    assert_eq!(changed["team_id"], Value::Null);
    assert_eq!(changed["changes"], json!({}));

    // Neither password is anywhere in the row, which is the whole point.
    let rendered = serde_json::to_string(&own).expect("the listing must serialize");
    assert!(!rendered.contains(PASSWORD), "{rendered}");
    assert!(!rendered.contains("a far better password"), "{rendered}");
}

#[tokio::test]
async fn reading_the_team_log_takes_the_admin_role() {
    let Some((_database, cast)) = open("audit_flow::authorization").await else {
        return;
    };

    // A plain member of the same team, invited without the admin role.
    let guest_email = format!("guest-{}@example.com", Uuid::new_v4());
    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&json!({
            "email": guest_email,
            "team_id": cast.team,
            "roles": [ "editor" ],
        })),
        Some(&cast.owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let guest = register(&cast.harness.router, &guest_email).await;
    let token = invitation_token(&cast.harness.outbox, &guest_email);
    let (status, _headers, body) = send(
        &cast.harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&guest),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let path = format!("/account/teams/{}/audit-events", cast.team);
    let (status, _headers, body) =
        send(&cast.harness.router, "GET", &path, None, Some(&guest)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    // A stranger is a 404 rather than a 403: the guard refuses before the role
    // is read, so the team's existence is not revealed.
    let stranger_email = format!("stranger-{}@example.com", Uuid::new_v4());
    let stranger = register(&cast.harness.router, &stranger_email).await;
    let (status, _headers, body) =
        send(&cast.harness.router, "GET", &path, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");

    // And nobody at all is a 401.
    let (status, _headers, body) = send(&cast.harness.router, "GET", &path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

#[tokio::test]
async fn the_listing_pages_and_filters_by_subject_and_actor() {
    let Some((_database, cast)) = open("audit_flow::listing").await else {
        return;
    };

    // Five renames, so there is something to page through.
    for round in 1..=5 {
        let path = format!("/tenancy/teams/{}", cast.team);
        let (status, _headers, body) = send(
            &cast.harness.router,
            "PATCH",
            &path,
            Some(&json!({ "name": format!("Team {round}") })),
            Some(&cast.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    let (status, first) = team_log(&cast, "?limit=2").await;
    assert_eq!(status, StatusCode::OK, "body: {first}");
    assert_eq!(actions(&first).len(), 2, "body: {first}");
    assert_eq!(first["pagination"]["total_items"], 5);
    assert_eq!(first["pagination"]["total_pages"], 3);
    // Newest first, so the last rename leads.
    assert_eq!(first["audit_events"][0]["subject_label"], "Team 5");

    let (status, second) = team_log(&cast, "?limit=2&page=2").await;
    assert_eq!(status, StatusCode::OK, "body: {second}");
    assert_eq!(second["audit_events"][0]["subject_label"], "Team 3");

    // A subject type nothing in this team has answers with nothing, rather
    // than with everything.
    let (status, none) = team_log(&cast, "?subject_type=CreativeConcept").await;
    assert_eq!(status, StatusCode::OK, "body: {none}");
    assert_eq!(none["pagination"]["total_items"], 0, "body: {none}");

    let (status, teams) = team_log(&cast, "?subject_type=Team").await;
    assert_eq!(status, StatusCode::OK, "body: {teams}");
    assert_eq!(teams["pagination"]["total_items"], 5, "body: {teams}");

    let actor = first["audit_events"][0]["user_id"]
        .as_str()
        .expect("a signed-in act names its actor")
        .to_owned();
    let (status, mine) = team_log(&cast, &format!("?actor_id={actor}")).await;
    assert_eq!(status, StatusCode::OK, "body: {mine}");
    assert_eq!(mine["pagination"]["total_items"], 5, "body: {mine}");

    let (status, nobody) = team_log(&cast, &format!("?actor_id={}", Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::OK, "body: {nobody}");
    assert_eq!(nobody["pagination"]["total_items"], 0, "body: {nobody}");
}

#[tokio::test]
async fn a_rolled_back_write_records_nothing() {
    let Some(database) = TestDatabase::create("audit_flow::transaction").await else {
        return;
    };
    let pool = database.pool().await;
    let mut connection = pool
        .get()
        .await
        .expect("the test database must accept a connection");

    // The whole guarantee in one place: the event rides the caller's
    // connection, so a transaction that rolls back leaves no evidence of a
    // write that never happened.
    let outcome = diesel_async::AsyncConnection::transaction::<(), diesel::result::Error, _>(
        &mut connection,
        async |transaction| {
            audit::record(
                transaction,
                &Context::system(),
                &Event::new("rolled.back", "Nothing"),
            )
            .await?;
            Err(diesel::result::Error::RollbackTransaction)
        },
    )
    .await;
    assert!(outcome.is_err(), "the transaction must roll back");

    let recorded: i64 = audit_events::table
        .filter(audit_events::action.eq("rolled.back"))
        .count()
        .get_result(&mut connection)
        .await
        .expect("counting must succeed");
    assert_eq!(recorded, 0);

    // And a committed one keeps its row, actor and all.
    let id = audit::record(
        &mut connection,
        &Context::system(),
        &Event::new("committed", "Nothing")
            .label("a thing")
            .changes(Changes::new().field("state", "before", "after")),
    )
    .await
    .expect("recording must succeed");

    let stored: (Option<String>, String, Value) = audit_events::table
        .find(id)
        .select((
            audit_events::actor_name,
            audit_events::subject_type,
            audit_events::changes,
        ))
        .first(&mut connection)
        .await
        .expect("the row must be readable");

    // A system act names no actor, which is what the nullable column is for.
    assert_eq!(stored.0, None);
    assert_eq!(stored.1, "Nothing");
    assert_eq!(
        stored.2["state"],
        json!({ "old": "before", "new": "after" })
    );
}
