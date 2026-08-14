//! Billing end to end: the free plan, checkout, and the customer portal.
//!
//! Stripe cannot run in a test, so this test *is* Stripe: a small axum server
//! answers the three calls the framework makes, in the shapes Stripe answers
//! them in. The framework is pointed at it with `STRIPE_API_BASE`, which is
//! the same escape hatch a Stripe-compatible mock uses, so nothing about the
//! code under test is special-cased for the test. That makes the whole path
//! real: the plan resolved from configuration, the customer created and
//! stored, the checkout session opened with the price the plan names, and the
//! portal session opened for the customer that was stored.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use std::sync::{Arc, Mutex, MutexGuard};

use anubis::billing::PlanSet;
use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::schema::{organization_memberships, organizations, subscriptions};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use support::{Harness, TestDatabase, register, send};
use uuid::Uuid;

/// The starter's role vocabulary, which is what bootstrapping needs.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  editor:
    includes: [default]
    models: {}
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [editor, billing]
    models:
      Team: [manage]
";

/// Two plans: the free one every organization starts on, and one to buy.
const BILLING_YML: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 1
  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
      yearly:
        stripe_price_id: price_pro_yearly
        amount: 29000
        currency: usd
    limits:
      seats: 25
";

/// The customer the mock Stripe hands out.
const CUSTOMER_ID: &str = "cus_test_acme";

/// One request the mock Stripe took, kept as it arrived.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: HeaderMap,
    body: String,
}

type Log = Arc<Mutex<Vec<Received>>>;

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// Serves the three Stripe endpoints the framework calls.
///
/// Returns its base URL and the log of everything it took, so the test can
/// assert on the exact parameters that were sent.
async fn start_mock_stripe() -> (String, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    let router = Router::new()
        .route(
            "/v1/customers",
            post(
                async |State(log): State<Log>, headers: HeaderMap, body: String| {
                    record(&log, "/v1/customers", &headers, &body);
                    Json(json!({ "id": CUSTOMER_ID, "object": "customer" }))
                },
            ),
        )
        .route(
            "/v1/checkout/sessions",
            post(
                async |State(log): State<Log>, headers: HeaderMap, body: String| {
                    record(&log, "/v1/checkout/sessions", &headers, &body);
                    Json(json!({
                        "id": "cs_test_session",
                        "object": "checkout.session",
                        "url": "https://checkout.stripe.com/c/pay/cs_test_session",
                    }))
                },
            ),
        )
        .route(
            "/v1/billing_portal/sessions",
            post(
                async |State(log): State<Log>, headers: HeaderMap, body: String| {
                    record(&log, "/v1/billing_portal/sessions", &headers, &body);
                    Json(json!({
                        "id": "bps_test_session",
                        "object": "billing_portal.session",
                        "url": "https://billing.stripe.com/p/session/bps_test_session",
                    }))
                },
            ),
        )
        .with_state(Arc::clone(&log));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the mock must bind a loopback port");
    let address = listener
        .local_addr()
        .expect("the mock must report its address");
    tokio::spawn(async move {
        let _served = axum::serve(listener, router).await;
    });

    (format!("http://{address}"), log)
}

fn record(log: &Log, path: &str, headers: &HeaderMap, body: &str) {
    unlock(log).push(Received {
        path: path.to_owned(),
        headers: headers.clone(),
        body: body.to_owned(),
    });
}

/// The one request the mock took for `path`.
fn only_call(log: &Log, path: &str) -> Received {
    let calls: Vec<Received> = unlock(log)
        .iter()
        .filter(|call| call.path == path)
        .cloned()
        .collect();
    assert_eq!(calls.len(), 1, "expected exactly one call to {path}");
    calls
        .into_iter()
        .next()
        .expect("the length was just asserted")
}

/// The application's config, pointed at the mock Stripe.
fn config(api_base: &str) -> AppConfig {
    let api_base = api_base.to_owned();
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("https://app.example.com".to_owned()),
        "STRIPE_SECRET_KEY" => Some("sk_test_billing_flow".to_owned()),
        "STRIPE_API_BASE" => Some(api_base.clone()),
        _other => None,
    })
    .expect("the test config must parse")
}

/// The billing router, mounted where an application mounts it.
///
/// Over the same database the harness serves, so the accounts registered
/// through the harness are the accounts these routes authorize.
fn billing_router(pool: anubis::db::DbPool, config: &AppConfig) -> Router {
    let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let plans = PlanSet::from_yaml(BILLING_YML).expect("plans must parse");
    Router::new().nest(
        "/billing",
        anubis::billing::router(pool, roles, plans, config),
    )
}

/// Writes one subscription row, the way the next slab's webhook will.
///
/// Returns the insert's own result rather than unwrapping it, because two of
/// its callers are asserting that the database refuses.
async fn insert_subscription(
    connection: &mut diesel_async::AsyncPgConnection,
    organization_id: Uuid,
    stripe_subscription_id: &str,
    status: &str,
) -> QueryResult<usize> {
    diesel::insert_into(subscriptions::table)
        .values((
            subscriptions::organization_id.eq(organization_id),
            subscriptions::plan_key.eq("pro"),
            subscriptions::stripe_subscription_id.eq(stripe_subscription_id.to_owned()),
            subscriptions::status.eq(status.to_owned()),
            subscriptions::billing_interval.eq("monthly"),
        ))
        .execute(connection)
        .await
}

/// The organization the registered account was bootstrapped into.
async fn bootstrapped_organization(router: &Router, cookie: &str) -> Uuid {
    let (status, _headers, body) =
        send(router, "GET", "/tenancy/memberships", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["organizations"][0]["id"]
        .as_str()
        .expect("registration bootstraps one organization")
        .parse()
        .expect("an organization id is a UUID")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear narrative from the free plan through checkout to cancellation"
)]
async fn an_organization_starts_free_buys_a_plan_and_manages_it() {
    let Some(database) = TestDatabase::create("billing_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, stripe_log) = start_mock_stripe().await;
    let config = config(&stripe_base);
    let billing = billing_router(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let path = format!("/billing/organizations/{organization_id}");

    // 1. A brand new organization is on the free plan, with no subscription and
    //    no call to Stripe.
    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert_eq!(body["plan"]["limits"]["seats"], 1, "body: {body}");
    assert!(body["subscription"].is_null(), "body: {body}");
    assert_eq!(body["billing_enabled"], true, "body: {body}");
    assert!(unlock(&stripe_log).is_empty(), "nothing was bought yet");

    // 2. Checkout creates the customer and opens a session for the price the
    //    plan names.
    let purchase = json!({ "plan_key": "pro", "interval": "monthly" });
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/checkout"),
        Some(&purchase),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["url"], "https://checkout.stripe.com/c/pay/cs_test_session",
        "the caller is handed Stripe's own URL: body: {body}",
    );

    let customer_call = only_call(&stripe_log, "/v1/customers");
    assert!(
        customer_call
            .headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|key| key.contains(&organization_id.to_string())),
        "the customer create is idempotent per organization: {:?}",
        customer_call.headers,
    );
    assert!(
        customer_call
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "Bearer sk_test_billing_flow"),
        "the secret key authenticates the call",
    );
    assert!(
        customer_call.body.contains("metadata%5Borganization_id%5D"),
        "the customer carries its organization: {}",
        customer_call.body,
    );

    let checkout_call = only_call(&stripe_log, "/v1/checkout/sessions");
    for expected in [
        "mode=subscription",
        "customer=cus_test_acme",
        "line_items%5B0%5D%5Bprice%5D=price_pro_monthly",
        "subscription_data%5Bmetadata%5D%5Borganization_id%5D",
        "metadata%5Bplan_key%5D=pro",
    ] {
        assert!(
            checkout_call.body.contains(expected),
            "expected {expected} in: {}",
            checkout_call.body,
        );
    }
    assert!(
        checkout_call
            .body
            .contains("success_url=https%3A%2F%2Fapp.example.com%2Forganizations%2F"),
        "the return URL is built from APP_URL: {}",
        checkout_call.body,
    );

    // The customer is stored, so a second checkout would not create another.
    let mut connection = harness.pool.get().await.expect("a connection");
    let stored: Option<String> = organizations::table
        .find(organization_id)
        .select(organizations::stripe_customer_id)
        .first(&mut connection)
        .await
        .expect("the organization must be readable");
    assert_eq!(stored.as_deref(), Some(CUSTOMER_ID));

    // 3. The portal opens for that stored customer.
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/portal"),
        Some(&json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["url"], "https://billing.stripe.com/p/session/bps_test_session",
        "body: {body}",
    );
    let portal_call = only_call(&stripe_log, "/v1/billing_portal/sessions");
    assert!(
        portal_call.body.contains("customer=cus_test_acme"),
        "the portal opens for the stored customer: {}",
        portal_call.body,
    );

    // 4. Once a subscription exists, the plan it names is the plan in force,
    //    and a second checkout is refused in favor of the portal.
    //
    //    The row is written here by hand because writing it from Stripe's
    //    events is the next slab's work; the shape is the one that lands.
    let period_end = Utc::now() + Duration::days(30);
    diesel::insert_into(subscriptions::table)
        .values((
            subscriptions::organization_id.eq(organization_id),
            subscriptions::plan_key.eq("pro"),
            subscriptions::stripe_subscription_id.eq("sub_test_acme"),
            subscriptions::status.eq("active"),
            subscriptions::billing_interval.eq("monthly"),
            subscriptions::quantity.eq(1),
            subscriptions::current_period_end.eq(period_end),
        ))
        .execute(&mut connection)
        .await
        .expect("the subscription must insert");

    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "pro", "body: {body}");
    assert_eq!(body["plan"]["limits"]["seats"], 25, "body: {body}");
    assert_eq!(body["subscription"]["status"], "active", "body: {body}");
    assert_eq!(
        body["subscription"]["billing_interval"], "monthly",
        "body: {body}",
    );

    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/checkout"),
        Some(&purchase),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("portal")),
        "the answer points at the portal: {body}",
    );

    // 5. Cancelling ends the subscription, and the organization is free again.
    diesel::update(subscriptions::table)
        .filter(subscriptions::organization_id.eq(organization_id))
        .set(subscriptions::status.eq("canceled"))
        .execute(&mut connection)
        .await
        .expect("the subscription must update");

    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert!(
        body["subscription"].is_null(),
        "a finished subscription is not the current one: {body}",
    );
}

#[tokio::test]
async fn checkout_refuses_a_plan_nobody_can_buy() {
    let Some(database) = TestDatabase::create("billing_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, stripe_log) = start_mock_stripe().await;
    let config = config(&stripe_base);
    let billing = billing_router(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let checkout = format!("/billing/organizations/{organization_id}/checkout");

    for (body, reason) in [
        (
            json!({ "plan_key": "enterprise", "interval": "monthly" }),
            "No plan named",
        ),
        (
            json!({ "plan_key": "free", "interval": "monthly" }),
            "needs no checkout",
        ),
        (
            json!({ "plan_key": "pro", "interval": "weekly" }),
            "monthly",
        ),
    ] {
        let (status, _headers, answered) =
            send(&billing, "POST", &checkout, Some(&body), Some(&owner)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {body}: {answered}");
        assert!(
            answered["message"]
                .as_str()
                .is_some_and(|message| message.contains(reason)),
            "expected {reason:?} for {body}: {answered}",
        );
    }

    // The portal has nothing to open before a customer exists.
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("/billing/organizations/{organization_id}/portal"),
        Some(&json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    assert!(
        unlock(&stripe_log).is_empty(),
        "a request Stripe would refuse never reaches Stripe",
    );
}

#[tokio::test]
async fn only_members_who_may_spend_money_reach_the_write_endpoints() {
    let Some(database) = TestDatabase::create("billing_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, _stripe_log) = start_mock_stripe().await;
    let config = config(&stripe_base);
    let billing = billing_router(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let path = format!("/billing/organizations/{organization_id}");

    // A member of the organization who holds neither admin nor billing.
    let plain_email = format!("plain-{}@example.com", Uuid::new_v4());
    let plain = register(&harness.router, &plain_email).await;
    let plain_id: Uuid = anubis::schema::users::table
        .filter(anubis::schema::users::email.eq(&plain_email))
        .select(anubis::schema::users::id)
        .first(&mut harness.pool.get().await.expect("a connection"))
        .await
        .expect("the account must exist");
    diesel::insert_into(organization_memberships::table)
        .values((
            organization_memberships::organization_id.eq(organization_id),
            organization_memberships::user_id.eq(plain_id),
            organization_memberships::roles.eq(vec!["default".to_owned()]),
        ))
        .execute(&mut harness.pool.get().await.expect("a connection"))
        .await
        .expect("the membership must insert");

    // Reading is open to every member: the plan explains what the whole
    // organization can do.
    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&plain)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Spending is not.
    for endpoint in ["checkout", "portal"] {
        let (status, _headers, body) = send(
            &billing,
            "POST",
            &format!("{path}/{endpoint}"),
            Some(&json!({ "plan_key": "pro", "interval": "monthly" })),
            Some(&plain),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "for {endpoint}: {body}");
    }

    // Somebody else's organization does not exist as far as a stranger is
    // concerned, on any of the three routes.
    let stranger = register(
        &harness.router,
        &format!("stranger-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/checkout"),
        Some(&json!({ "plan_key": "pro", "interval": "monthly" })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");

    // And nobody at all gets nothing at all.
    let (status, _headers, body) = send(&billing, "GET", &path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

#[tokio::test]
async fn without_a_stripe_key_reading_works_and_buying_says_so() {
    let Some(database) = TestDatabase::create("billing_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    // The zero-configuration shape: no STRIPE_SECRET_KEY anywhere.
    let unconfigured = AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _other => None,
    })
    .expect("the test config must parse");
    let billing = billing_router(harness.pool.clone(), &unconfigured);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let path = format!("/billing/organizations/{organization_id}");

    let (status, _headers, body) = send(&billing, "GET", &path, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert_eq!(
        body["billing_enabled"], false,
        "the UI is told billing is off: {body}",
    );

    for (endpoint, body) in [
        (
            "checkout",
            json!({ "plan_key": "pro", "interval": "monthly" }),
        ),
        ("portal", json!({})),
    ] {
        let (status, _headers, answered) = send(
            &billing,
            "POST",
            &format!("{path}/{endpoint}"),
            Some(&body),
            Some(&owner),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "for {endpoint}: {answered}",
        );
        assert!(
            answered["message"]
                .as_str()
                .is_some_and(|message| message.contains("STRIPE_SECRET_KEY")),
            "the answer names the variable to set: {answered}",
        );
    }
}

/// A subscription belongs to exactly one organization at a time.
///
/// The database enforces it rather than the handler, because two events
/// arriving at once would otherwise each write a row and neither would know.
#[tokio::test]
async fn an_organization_holds_one_live_subscription_at_a_time() {
    let Some(database) = TestDatabase::create("billing_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let mut connection = harness.pool.get().await.expect("a connection");

    insert_subscription(&mut connection, organization_id, "sub_first", "active")
        .await
        .expect("the first subscription must insert");
    insert_subscription(&mut connection, organization_id, "sub_second", "trialing")
        .await
        .expect_err("a second live subscription must be refused");

    // A subscription that is over holds no slot, so the next one may start.
    diesel::update(subscriptions::table)
        .filter(subscriptions::stripe_subscription_id.eq("sub_first"))
        .set(subscriptions::status.eq("canceled"))
        .execute(&mut connection)
        .await
        .expect("the subscription must update");
    insert_subscription(&mut connection, organization_id, "sub_second", "trialing")
        .await
        .expect("a new subscription may start once the old one ended");

    let live: Value = json!(
        anubis::billing::Subscription::current_for_organization(&mut connection, organization_id)
            .await
            .expect("the query must run")
            .map(|subscription| subscription.stripe_subscription_id)
    );
    assert_eq!(live, json!("sub_second"));
}
