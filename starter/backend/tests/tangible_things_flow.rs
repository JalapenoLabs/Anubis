//! The `TangibleThing` slice, end to end against a real Postgres database.
//!
//! One narrative covers everything a nested scaffolded model owes its
//! application: create under a parent, list with the locked pagination
//! envelope, refuse another tenant's records with `404`, refuse a read-only
//! member's writes with `403`, refuse a parent from another team, update, and
//! destroy, on the account routes and then on `/api/v1` with a platform
//! application's bearer token. When `anubis scaffold model` transforms the
//! living template, it transforms this narrative with it, so every generated
//! model arrives with the same proof.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{
    boot, bootstrapped_team, invite_and_claim, platform_token, register, send, send_as_token,
};
use uuid::Uuid;

/// Creates a parent creative concept in `team_id` and returns its id.
async fn create_parent(router: &axum::Router, cookie: &str, team_id: &str, name: &str) -> String {
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_tangible_thing_slice_serves_full_crud() {
    let Some((router, outbox)) = boot().await else {
        eprintln!("skipping tangible_things_flow test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_email = format!("tangible-thing-owner-{run}@example.com");
    let outsider_email = format!("tangible-thing-outsider-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;
    let creative_concept_id = create_parent(&router, &owner_cookie, &team_id, "Lighthouse").await;
    let collection_path =
        format!("/account/creative-concepts/{creative_concept_id}/tangible-things");

    // Create under the parent; the parent comes from the route, never the body.
    let (status, body) = send(
        &router,
        "POST",
        &collection_path,
        Some(&json!({
            "name": "Fresnel lens",
            "description": "Casts the beam",
            // 🐺 anubis:test-create
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let tangible_thing_id = body["tangible_thing"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    assert_eq!(
        body["tangible_thing"]["creative_concept_id"],
        json!(creative_concept_id)
    );
    // 🐺 anubis:test-created

    // A blank name never reaches the database.
    let (status, _body) = send(
        &router,
        "POST",
        &collection_path,
        Some(&json!({ "name": "   " })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Listing follows the locked conventions: plural key, pagination envelope.
    let (status, body) = send(
        &router,
        "GET",
        &format!("{collection_path}?page=1&limit=25&sort=name"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["pagination"]["total_items"], json!(1));
    assert_eq!(
        body["tangible_things"][0]["name"],
        json!("Fresnel lens"),
        "the plural key carries the records: {body}",
    );

    // Another tenant's records are indistinguishable from records that do not
    // exist: both answer 404, through the parent and on the record itself.
    let member_path = format!("/account/tangible-things/{tangible_thing_id}");
    for path in [&collection_path, &member_path] {
        let (status, _body) = send(&router, "GET", path, None, Some(&outsider_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }
    let ghost = format!("/account/tangible-things/{}", Uuid::new_v4());
    let (status, _body) = send(&router, "GET", &ghost, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Signing out entirely is a 401, before any record is touched.
    let (status, _body) = send(&router, "GET", &member_path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A `default` member reads the team's records but writes none of them.
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

    let (status, body) = send(&router, "GET", &member_path, None, Some(&outsider_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, _body) = send(
        &router,
        "POST",
        &collection_path,
        Some(&json!({ "name": "Contraband" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({ "name": "Contraband" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _body) = send(
        &router,
        "DELETE",
        &member_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Update: the trigger keeps updated_at moving, and a blank description
    // clears the column.
    let (status, body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({
            "name": "First-order lens",
            "description": "  ",
            // 🐺 anubis:test-update
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["tangible_thing"]["name"], json!("First-order lens"));
    assert_eq!(body["tangible_thing"]["description"], Value::Null);
    assert_ne!(
        body["tangible_thing"]["updated_at"], body["tangible_thing"]["created_at"],
        "the set_updated_at trigger must fire",
    );
    // 🐺 anubis:test-updated

    // A parent from another team is refused by the valid_* scoping method.
    let outsider_team = bootstrapped_team(&router, &outsider_cookie).await;
    let foreign_parent_id =
        create_parent(&router, &outsider_cookie, &outsider_team, "Elsewhere").await;
    let (status, _body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({ "creative_concept_id": foreign_parent_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cross-team parent refused");

    // Destroy, and the record is gone for good.
    let (status, _body) = send(&router, "DELETE", &member_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _body) = send(&router, "GET", &member_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // ---------------------------------------------------------------------
    // The same slice through `/api/v1`, where a platform application's bearer
    // token is the whole identity and its team is the whole ownership chain.
    // The columns are proven above: both surfaces share one serializer.
    // ---------------------------------------------------------------------
    let token = platform_token(&router, &owner_cookie, &team_id).await;
    let api_collection = format!("/api/v1/creative-concepts/{creative_concept_id}/tangible-things");

    // No token, and a forged one, are refused before any record is touched.
    for bearer in [None, Some("forged-token")] {
        let (status, _body) = send_as_token(&router, "GET", &api_collection, None, bearer).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let (status, body) = send_as_token(
        &router,
        "POST",
        &api_collection,
        Some(&json!({ "name": "Foghorn" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let api_id = body["tangible_thing"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    let api_member = format!("/api/v1/tangible-things/{api_id}");

    // The list envelope is the locked one, under the parent the route names.
    let (status, body) = send_as_token(
        &router,
        "GET",
        &format!("{api_collection}?limit=25&sort=name&name=Foghorn"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["pagination"]["total_items"], json!(1));
    assert_eq!(body["tangible_things"][0]["name"], json!("Foghorn"));

    let (status, body) = send_as_token(
        &router,
        "PATCH",
        &api_member,
        Some(&json!({ "name": "Foghorn mark II" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["tangible_thing"]["name"], json!("Foghorn mark II"));

    // Another team's token reaches neither the parent nor the record.
    let outsider_token = platform_token(&router, &outsider_cookie, &outsider_team).await;
    for path in [&api_collection, &api_member] {
        let (status, _body) =
            send_as_token(&router, "GET", path, None, Some(&outsider_token)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }

    let (status, _body) = send_as_token(&router, "DELETE", &api_member, None, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _body) = send_as_token(&router, "GET", &api_member, None, Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
