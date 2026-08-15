//! What a new account joins, under each bootstrap mode.
//!
//! Each test boots the framework over a database of its own and reads the
//! tenancy back the way the SPA does, through `GET /tenancy/memberships`. The
//! organizations table is counted alongside, because "one shared
//! organization" is a claim about a total rather than about what one account
//! can see.
//!
//! The OAuth half of the same promise lives in `oauth_flow`, beside the mock
//! provider that makes an OAuth signup possible at all.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::schema::organizations;
use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};

use support::{Harness, TestDatabase, register, send};

/// The organization a shared deployment in this suite puts everybody in.
const SHARED_ORGANIZATION: &str = "Acme Corporation";

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

/// How many organizations exist in the whole database.
async fn organization_count(harness: &Harness) -> i64 {
    let mut connection = harness.pool.get().await.expect("a connection");
    organizations::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run")
}

/// The default: two accounts are two tenants, each administering its own.
#[tokio::test]
async fn a_personal_bootstrap_gives_every_account_a_tenancy_of_its_own() {
    let Some(database) = TestDatabase::create("bootstrap_modes_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let ada = register(&harness.router, "ada@example.com").await;
    let grace = register(&harness.router, "grace@example.com").await;

    let ada_organizations = memberships(&harness, &ada).await;
    assert_eq!(
        ada_organizations.as_array().map(Vec::len),
        Some(1),
        "one organization: {ada_organizations}",
    );
    assert_eq!(
        ada_organizations[0]["name"],
        json!("ada"),
        "named after the address's local part",
    );
    assert_eq!(ada_organizations[0]["roles"], json!(["admin"]));
    assert_eq!(
        ada_organizations[0]["teams"].as_array().map(Vec::len),
        Some(1),
        "one team: {ada_organizations}",
    );
    assert_eq!(ada_organizations[0]["teams"][0]["name"], json!("General"));
    assert_eq!(ada_organizations[0]["teams"][0]["roles"], json!(["admin"]));

    let grace_organizations = memberships(&harness, &grace).await;
    assert_eq!(grace_organizations[0]["name"], json!("grace"));
    assert_ne!(
        grace_organizations[0]["id"], ada_organizations[0]["id"],
        "two accounts are two tenants",
    );

    assert_eq!(organization_count(&harness).await, 2);
}

/// A shared deployment: the first signup creates the organization everybody
/// else joins, and a joiner lands in it with the baseline role.
#[tokio::test]
async fn a_shared_bootstrap_puts_every_account_in_the_named_organization() {
    let Some(database) = TestDatabase::create("bootstrap_modes_flow").await else {
        return;
    };
    let harness = Harness::boot_with_config(
        &database,
        Harness::config_with(&[
            ("ANUBIS_BOOTSTRAP", "shared"),
            ("ANUBIS_SHARED_ORGANIZATION", SHARED_ORGANIZATION),
        ]),
    )
    .await;

    let founder = register(&harness.router, "founder@example.com").await;
    let joiner = register(&harness.router, "joiner@example.com").await;

    let founder_organizations = memberships(&harness, &founder).await;
    assert_eq!(
        founder_organizations[0]["name"],
        json!(SHARED_ORGANIZATION),
        "the organization configuration named, not one named after an address",
    );
    assert_eq!(
        founder_organizations[0]["roles"],
        json!(["admin"]),
        "whoever creates an organization administers it, so a shared \
         deployment is administrable from its first signup",
    );

    let joiner_organizations = memberships(&harness, &joiner).await;
    assert_eq!(
        joiner_organizations.as_array().map(Vec::len),
        Some(1),
        "one organization: {joiner_organizations}",
    );
    assert_eq!(
        joiner_organizations[0]["id"], founder_organizations[0]["id"],
        "the same organization, not a second one of the same name",
    );
    assert_eq!(
        joiner_organizations[0]["teams"][0]["id"], founder_organizations[0]["teams"][0]["id"],
        "and its default team",
    );
    assert_eq!(
        joiner_organizations[0]["roles"],
        json!(["default"]),
        "a joiner reads what the organization holds and nothing more",
    );
    assert_eq!(
        joiner_organizations[0]["teams"][0]["roles"],
        json!(["default"])
    );

    assert_eq!(
        organization_count(&harness).await,
        1,
        "a shared deployment has exactly the one organization it named",
    );
}
