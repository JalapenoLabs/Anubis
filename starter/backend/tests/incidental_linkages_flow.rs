//! The `IncidentalLinkage` association, end to end against a real Postgres
//! database.
//!
//! One narrative covers everything a scaffolded join owes its application:
//! options scoped to the caller's team, attach, list through the join, attach
//! again as a no-op, detach, a read-only member refused, another tenant's
//! owner hidden behind `404`, and another tenant's record refused on attach.
//! When `anubis scaffold join` transforms the living template, it transforms
//! this narrative with it, so every generated join arrives with the same proof.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{boot, bootstrapped_team, invite_and_claim, register, send};
use uuid::Uuid;

/// Creates the owning creative concept and returns its id.
async fn create_creative_concept(
    router: &axum::Router,
    cookie: &str,
    team_id: &str,
    name: &str,
) -> String {
    let (status, body) = send(
        router,
        "POST",
        &format!("/account/teams/{team_id}/creative-concepts"),
        Some(&json!({ "name": name })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned()
}

/// Creates a peripheral notion of `team_id` and returns its id.
async fn create_peripheral_notion(
    router: &axum::Router,
    cookie: &str,
    team_id: &str,
    name: &str,
) -> String {
    let (status, body) = send(
        router,
        "POST",
        &format!("/account/teams/{team_id}/peripheral-notions"),
        Some(&json!({ "name": name })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    body["peripheral_notion"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_incidental_linkage_association_attaches_and_detaches() {
    let Some((router, outbox)) = boot().await else {
        eprintln!("skipping incidental_linkages_flow test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_email = format!("incidental-linkage-owner-{run}@example.com");
    let outsider_email = format!("incidental-linkage-outsider-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;

    let creative_concept_id =
        create_creative_concept(&router, &owner_cookie, &team_id, "Lighthouse").await;
    let first_id = create_peripheral_notion(&router, &owner_cookie, &team_id, "Coastal").await;
    let second_id = create_peripheral_notion(&router, &owner_cookie, &team_id, "Historic").await;
    let links_path = format!("/account/creative-concepts/{creative_concept_id}/peripheral-notions");

    // The options a form may offer are the team's own records, and nothing else.
    let (status, body) = send(
        &router,
        "GET",
        &format!("/account/teams/{team_id}/incidental-linkages/options"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["options"].as_array().map(Vec::len), Some(2));
    assert_eq!(body["options"][0]["label"], json!("Coastal"));
    assert_eq!(body["options"][0]["value"], json!(first_id));

    // Attaching, then reading back through the join.
    let (status, _body) = send(
        &router,
        "POST",
        &links_path,
        Some(&json!({ "peripheral_notion_id": first_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send(&router, "GET", &links_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["peripheral_notions"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["peripheral_notions"][0]["id"], json!(first_id));

    // The unique pair makes a repeated attach a no-op rather than a duplicate.
    let (status, _body) = send(
        &router,
        "POST",
        &links_path,
        Some(&json!({ "peripheral_notion_id": first_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&router, "GET", &links_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["peripheral_notions"].as_array().map(Vec::len), Some(1));

    // A second record joins the first.
    let (status, _body) = send(
        &router,
        "POST",
        &links_path,
        Some(&json!({ "peripheral_notion_id": second_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&router, "GET", &links_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["peripheral_notions"].as_array().map(Vec::len), Some(2));

    // Another tenant sees the owner's record exactly as a missing one.
    let (status, _body) = send(&router, "GET", &links_path, None, Some(&outsider_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _body) = send(&router, "GET", &links_path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A record from another team is refused, which is the whole point of the
    // join model's `valid_*` scoping method.
    let outsider_team = bootstrapped_team(&router, &outsider_cookie).await;
    let foreign_id =
        create_peripheral_notion(&router, &outsider_cookie, &outsider_team, "Elsewhere").await;
    let (status, _body) = send(
        &router,
        "POST",
        &links_path,
        Some(&json!({ "peripheral_notion_id": foreign_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cross-team record refused");

    // A `default` member reads the association but cannot change it: attaching
    // is an update on the model that owns the association.
    invite_and_claim(
        &router,
        &outbox,
        &owner_cookie,
        &outsider_cookie,
        &team_id,
        &outsider_email,
        &["default"],
    )
    .await;

    let (status, body) = send(&router, "GET", &links_path, None, Some(&outsider_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, _body) = send(
        &router,
        "POST",
        &links_path,
        Some(&json!({ "peripheral_notion_id": first_id })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _body) = send(
        &router,
        "DELETE",
        &format!("{links_path}/{first_id}"),
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Detaching leaves the records themselves alone.
    let (status, _body) = send(
        &router,
        "DELETE",
        &format!("{links_path}/{first_id}"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&router, "GET", &links_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["peripheral_notions"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["peripheral_notions"][0]["id"], json!(second_id));
}
