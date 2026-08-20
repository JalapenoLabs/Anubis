//! The `CreativeConcept` slice, end to end against a real Postgres database.
//!
//! One narrative covers everything a team-owned scaffolded model owes its
//! application: create, list with the locked pagination envelope, refuse
//! another tenant's records with `404`, refuse an anonymous caller with `401`,
//! refuse a read-only member's writes with `403`, update, and destroy, on the
//! account routes and then on `/api/v1` with a platform application's bearer
//! token. When `anubis scaffold model` transforms the living template, it
//! transforms this narrative with it, so every generated model arrives with
//! the same proof.
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_creative_concept_slice_serves_full_crud() {
    let Some((router, outbox)) = boot().await else {
        eprintln!("skipping creative_concepts_flow test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_email = format!("creative-concept-owner-{run}@example.com");
    let outsider_email = format!("creative-concept-outsider-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;
    let collection_path = format!("/account/teams/{team_id}/creative-concepts");

    // Create. The owner registered as an admin, which includes editor, which
    // is granted `manage` on the model.
    let (status, body) = send(
        &router,
        "POST",
        &collection_path,
        Some(&json!({
            "name": "Lighthouse",
            "description": "Casts the beam",
            // 🐺 anubis:test-create
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let creative_concept_id = body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    assert_eq!(body["creative_concept"]["name"], json!("Lighthouse"));
    assert_eq!(
        body["creative_concept"]["description"],
        json!("Casts the beam")
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

    for name in ["Beacon", "Compass"] {
        let (status, body) = send(
            &router,
            "POST",
            &collection_path,
            Some(&json!({ "name": name })),
            Some(&owner_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
    }

    // Listing follows the locked conventions: plural key, pagination envelope,
    // 1-based pages, whitelisted sort, per-field filter.
    let (status, body) = send(
        &router,
        "GET",
        &format!("{collection_path}?page=1&limit=2&sort=name"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page = body["creative_concepts"]
        .as_array()
        .expect("the plural key carries the records");
    assert_eq!(page.len(), 2);
    assert_eq!(page[0]["name"], json!("Beacon"));
    assert_eq!(page[1]["name"], json!("Compass"));
    assert_eq!(body["pagination"]["page"], json!(1));
    assert_eq!(body["pagination"]["limit"], json!(2));
    assert_eq!(body["pagination"]["total_items"], json!(3));
    assert_eq!(body["pagination"]["total_pages"], json!(2));

    let (status, body) = send(
        &router,
        "GET",
        &format!("{collection_path}?page=2&limit=2&sort=name&name=house"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["pagination"]["total_items"],
        json!(1),
        "filtered count"
    );
    assert!(
        body["creative_concepts"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "page 2 of a single filtered match is empty: {body}",
    );

    // Another tenant's records are indistinguishable from records that do not
    // exist: both answer 404.
    let member_path = format!("/account/creative-concepts/{creative_concept_id}");
    for path in [&collection_path, &member_path] {
        let (status, _body) = send(&router, "GET", path, None, Some(&outsider_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }
    let ghost = format!("/account/creative-concepts/{}", Uuid::new_v4());
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
            "name": "First-order lighthouse",
            "description": "  ",
            // 🐺 anubis:test-update
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["creative_concept"]["name"],
        json!("First-order lighthouse")
    );
    assert_eq!(body["creative_concept"]["description"], Value::Null);
    assert_ne!(
        body["creative_concept"]["updated_at"], body["creative_concept"]["created_at"],
        "the set_updated_at trigger must fire",
    );
    // 🐺 anubis:test-updated

    // 🐺 anubis:test-associations

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
    let api_collection = "/api/v1/creative-concepts";

    // No token, and a forged one, are refused before any record is touched.
    for bearer in [None, Some("forged-token")] {
        let (status, _body) = send_as_token(&router, "GET", api_collection, None, bearer).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let (status, body) = send_as_token(
        &router,
        "POST",
        api_collection,
        Some(&json!({ "name": "Foghorn" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let api_id = body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    let api_member = format!("{api_collection}/{api_id}");

    // The list envelope is the locked one, and it is scoped to the token's team.
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
    assert_eq!(body["creative_concepts"][0]["name"], json!("Foghorn"));

    let (status, body) = send_as_token(
        &router,
        "PATCH",
        &api_member,
        Some(&json!({ "name": "Foghorn mark II" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["creative_concept"]["name"], json!("Foghorn mark II"));

    // Another team's token is indistinguishable from a missing record.
    let outsider_team = bootstrapped_team(&router, &outsider_cookie).await;
    let outsider_token = platform_token(&router, &outsider_cookie, &outsider_team).await;
    let (status, _body) =
        send_as_token(&router, "GET", &api_member, None, Some(&outsider_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "leaked {api_member}");

    let (status, _body) = send_as_token(&router, "DELETE", &api_member, None, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _body) = send_as_token(&router, "GET", &api_member, None, Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Every write publishes its event to the team's webhook endpoints.
///
/// The assertions stop at the delivery row, which is where the model's own
/// responsibility ends: emitting inside the write's transaction, with the same
/// serialized shape the endpoints answer with. Signing the request and posting
/// it is the framework's job, driven by a worker no test binary runs, and
/// `anubis/tests/webhooks_flow.rs` proves that half against a real listener.
#[tokio::test]
async fn writing_a_creative_concept_queues_a_webhook_delivery() {
    let Some((router, _outbox)) = boot().await else {
        eprintln!("skipping creative_concepts_flow webhook test: DATABASE_URL is not set");
        return;
    };

    let run = Uuid::new_v4();
    let owner_cookie = register(
        &router,
        &format!("creative-concept-hooks-{run}@example.com"),
    )
    .await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;

    // Subscribe to the model's whole lifecycle. The secret is shown once here
    // and nowhere else, which is what makes signing verifiable at the receiver.
    let (status, body) = send(
        &router,
        "POST",
        &format!("/developers/teams/{team_id}/webhook-endpoints"),
        Some(&json!({
            "url": "https://receiver.example.com/anubis",
            "description": "Creative concept lifecycle",
            "event_types": [
                "creative_concept.created",
                "creative_concept.updated",
                "creative_concept.destroyed",
            ],
        })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body["secret"]
            .as_str()
            .is_some_and(|secret| secret.starts_with("whsec_")),
        "the signing secret is returned exactly once: {body}",
    );
    let endpoint_id = body["webhook_endpoint"]["id"]
        .as_str()
        .expect("the response carries the endpoint")
        .to_owned();
    let deliveries_path =
        format!("/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries");

    let (status, body) = send(
        &router,
        "POST",
        &format!("/account/teams/{team_id}/creative-concepts"),
        Some(&json!({ "name": "Signal fire" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let creative_concept_id = body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    let member_path = format!("/account/creative-concepts/{creative_concept_id}");

    let (status, _body) = send(
        &router,
        "PATCH",
        &member_path,
        Some(&json!({ "name": "Signal fire mark II" })),
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
            "creative_concept.destroyed",
            "creative_concept.updated",
            "creative_concept.created",
        ],
        "every write publishes its event: {body}",
    );

    // The payload is the record as the endpoints serialize it, not an id.
    let created = deliveries.last().expect("the create is the oldest");
    assert_eq!(created["status"], json!("pending"));
    assert_eq!(created["attempts"], json!(0));
    assert_eq!(created["payload"]["id"], json!(creative_concept_id));
    assert_eq!(created["payload"]["name"], json!("Signal fire"));
    assert_eq!(
        deliveries[0]["payload"]["name"],
        json!("Signal fire mark II"),
        "the destroy carries the record as it last stood: {body}",
    );
}
