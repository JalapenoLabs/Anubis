//! The `GranularDetail` slice, end to end against a real Postgres database.
//!
//! One narrative covers everything a three-deep scaffolded model owes its
//! application: create under a parent that is itself owned through a parent,
//! list with the locked pagination envelope, refuse another tenant's records
//! with `404` at every hop of the chain, refuse a read-only member's writes
//! with `403`, refuse a parent from another team, update, and destroy, on the
//! account routes and then on `/api/v1` with a platform application's bearer
//! token. When `anubis scaffold model` transforms the living template, it
//! transforms this narrative with it, so every generated model arrives with the
//! same proof.
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

/// Creates the chain's root, a creative concept in `team_id`, and returns its id.
async fn create_grandparent(
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

/// Creates a parent tangible thing under `creative_concept_id`.
async fn create_parent(
    router: &axum::Router,
    cookie: &str,
    creative_concept_id: &str,
    name: &str,
) -> String {
    let (status, body) = send(
        router,
        "POST",
        &format!("/account/creative-concepts/{creative_concept_id}/tangible-things"),
        Some(&json!({ "name": name })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    body["tangible_thing"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_granular_detail_slice_serves_full_crud() {
    let Some((router, outbox)) = boot().await else {
        eprintln!("skipping granular_details_flow test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_email = format!("granular-detail-owner-{run}@example.com");
    let outsider_email = format!("granular-detail-outsider-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;
    let creative_concept_id =
        create_grandparent(&router, &owner_cookie, &team_id, "Lighthouse").await;
    let tangible_thing_id =
        create_parent(&router, &owner_cookie, &creative_concept_id, "Fresnel lens").await;
    let collection_path = format!("/account/tangible-things/{tangible_thing_id}/granular-details");

    // Create under the parent; the parent comes from the route, never the body.
    let (status, body) = send(
        &router,
        "POST",
        &collection_path,
        Some(&json!({
            "name": "Focal length",
            "description": "Measured from the centre",
            // 🐺 anubis:test-create
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let granular_detail_id = body["granular_detail"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    assert_eq!(
        body["granular_detail"]["tangible_thing_id"],
        json!(tangible_thing_id)
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
        body["granular_details"][0]["name"],
        json!("Focal length"),
        "the plural key carries the records: {body}",
    );

    // Another tenant's records are indistinguishable from records that do not
    // exist: both answer 404, through the parent and on the record itself. The
    // chain is walked whole, so a crafted path is refused at every hop rather
    // than at the first one.
    let member_path = format!("/account/granular-details/{granular_detail_id}");
    for path in [&collection_path, &member_path] {
        let (status, _body) = send(&router, "GET", path, None, Some(&outsider_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }
    let ghost = format!("/account/granular-details/{}", Uuid::new_v4());
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
            "name": "Focal distance",
            "description": "  ",
            // 🐺 anubis:test-update
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["granular_detail"]["name"], json!("Focal distance"));
    assert_eq!(body["granular_detail"]["description"], Value::Null);
    assert_ne!(
        body["granular_detail"]["updated_at"], body["granular_detail"]["created_at"],
        "the set_updated_at trigger must fire",
    );
    // 🐺 anubis:test-updated

    // 🐺 anubis:test-associations

    // A parent from another team is refused by the valid_* scoping method,
    // which reaches the team through the parent's own parent rather than
    // through a column on the parent.
    let outsider_team = bootstrapped_team(&router, &outsider_cookie).await;
    let foreign_grandparent_id =
        create_grandparent(&router, &outsider_cookie, &outsider_team, "Elsewhere").await;
    let foreign_parent_id = create_parent(
        &router,
        &outsider_cookie,
        &foreign_grandparent_id,
        "Somewhere else",
    )
    .await;
    let (status, _body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({ "tangible_thing_id": foreign_parent_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cross-team parent refused");

    // And the far end of that chain is refused too: creating under another
    // team's parent is a 404, not a 403, because the parent is unreachable.
    let (status, _body) = send(
        &router,
        "POST",
        &format!("/account/tangible-things/{foreign_parent_id}/granular-details"),
        Some(&json!({ "name": "Trespass" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-team parent refused");

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
    let api_collection = format!("/api/v1/tangible-things/{tangible_thing_id}/granular-details");

    // No token, and a forged one, are refused before any record is touched.
    for bearer in [None, Some("forged-token")] {
        let (status, _body) = send_as_token(&router, "GET", &api_collection, None, bearer).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let (status, body) = send_as_token(
        &router,
        "POST",
        &api_collection,
        Some(&json!({ "name": "Prism angle" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let api_id = body["granular_detail"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    let api_member = format!("/api/v1/granular-details/{api_id}");

    // The list envelope is the locked one, under the parent the route names.
    let (status, body) = send_as_token(
        &router,
        "GET",
        &format!("{api_collection}?limit=25&sort=name&name=Prism"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["pagination"]["total_items"], json!(1));
    assert_eq!(body["granular_details"][0]["name"], json!("Prism angle"));

    let (status, body) = send_as_token(
        &router,
        "PATCH",
        &api_member,
        Some(&json!({ "name": "Prism angle mark II" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["granular_detail"]["name"],
        json!("Prism angle mark II")
    );

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

/// Every write publishes its event to the team's webhook endpoints.
///
/// The team comes off the chain's root, two joins away, which is the one thing
/// this depth does differently from every other: an event is addressed to a
/// team the record itself never names.
#[tokio::test]
async fn writing_a_granular_detail_queues_a_webhook_delivery() {
    let Some((router, _outbox)) = boot().await else {
        eprintln!("skipping granular_details_flow webhook test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_cookie = register(&router, &format!("granular-detail-hooks-{run}@example.com")).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;
    let creative_concept_id =
        create_grandparent(&router, &owner_cookie, &team_id, "Lighthouse").await;
    let tangible_thing_id =
        create_parent(&router, &owner_cookie, &creative_concept_id, "Fresnel lens").await;

    // Subscribe to the model's whole lifecycle. The secret is shown once here
    // and nowhere else, which is what makes signing verifiable at the receiver.
    let (status, body) = send(
        &router,
        "POST",
        &format!("/developers/teams/{team_id}/webhook-endpoints"),
        Some(&json!({
            "url": "https://receiver.example.com/anubis",
            "description": "Granular detail lifecycle",
            "event_types": [
                "granular_detail.created",
                "granular_detail.updated",
                "granular_detail.destroyed",
            ],
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let endpoint_id = body["webhook_endpoint"]["id"]
        .as_str()
        .expect("the response carries the endpoint")
        .to_owned();
    let deliveries_path =
        format!("/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries");

    let (status, body) = send(
        &router,
        "POST",
        &format!("/account/tangible-things/{tangible_thing_id}/granular-details"),
        Some(&json!({ "name": "Focal length" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let granular_detail_id = body["granular_detail"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    let member_path = format!("/account/granular-details/{granular_detail_id}");

    let (status, _body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({ "name": "Focal length mark II" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _body) = send(&router, "DELETE", &member_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Newest first, so the log reads backwards through the record's life.
    let (status, body) = send(&router, "GET", &deliveries_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let deliveries = body["webhook_deliveries"]
        .as_array()
        .expect("the plural key carries the deliveries");
    let events: Vec<&str> = deliveries
        .iter()
        .filter_map(|delivery| delivery["event_type"].as_str())
        .collect();
    assert_eq!(
        events,
        vec![
            "granular_detail.destroyed",
            "granular_detail.updated",
            "granular_detail.created",
        ],
        "every write publishes its event: {body}",
    );

    // The payload is the record as the endpoints serialize it, not an id.
    let created = deliveries.last().expect("the create is the oldest");
    assert_eq!(created["status"], json!("pending"));
    assert_eq!(created["payload"]["id"], json!(granular_detail_id));
    assert_eq!(created["payload"]["name"], json!("Focal length"));
    assert_eq!(
        deliveries[0]["payload"]["name"],
        json!("Focal length mark II"),
        "the destroy carries the record as it last stood: {body}",
    );
}
