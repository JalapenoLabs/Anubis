//! The subscription lifecycle end to end: Stripe's events become rows.
//!
//! Stripe cannot run in a test, so this test *is* Stripe: a small axum server
//! answers the reads the framework makes, in the shapes Stripe answers them in,
//! and the events are signed with the same scheme and secret Stripe would use.
//! The framework is pointed at the mock with `STRIPE_API_BASE`, so nothing about
//! the code under test is special-cased for the test. What that buys is a real
//! path: a signed request, a stored event, a queued job, a read back from
//! Stripe, and a subscription row the billing endpoint then renders.
//!
//! Jobs are run by calling the handler directly rather than by starting a
//! worker, so each narrative stays deterministic. That the endpoint *queues*
//! them is asserted separately, against the `jobs` table.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use anubis::billing::{PlanSet, ProcessStripeEvent, Reconciler};
use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::schema::{jobs, organization_memberships, organizations, stripe_billing_events};
use anubis::webhooks::signature;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use support::{Harness, TestDatabase, register, send};
use tower::ServiceExt as _;
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

/// Three plans: the free one, and two that can be switched between.
const BILLING_YML: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 1
  - key: pro
    name: Pro
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
  - key: scale
    name: Scale
    prices:
      monthly:
        stripe_price_id: price_scale_monthly
        amount: 9900
        currency: usd
    limits:
      seats: 250
";

/// The secret this test's Stripe signs with.
const WEBHOOK_SECRET: &str = "whsec_billing_webhooks_flow";

/// Where the framework mounts its own receiver.
const RECEIVER: &str = "/webhooks/stripe-billing";

/// What the mock Stripe holds, keyed by subscription id.
type Subscriptions = Arc<Mutex<BTreeMap<String, Value>>>;

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// Serves the two subscription reads the lifecycle makes.
///
/// Returns its base URL and the store behind it, so a narrative can change what
/// Stripe says between events, which is the whole point of reading rather than
/// trusting an event's body.
async fn start_mock_stripe() -> (String, Subscriptions) {
    let held: Subscriptions = Arc::new(Mutex::new(BTreeMap::new()));

    let router = Router::new()
        .route("/v1/subscriptions", get(list_subscriptions))
        .route("/v1/subscriptions/{id}", get(retrieve_subscription))
        .with_state(Arc::clone(&held));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the mock must bind a loopback port");
    let address = listener
        .local_addr()
        .expect("the mock must report its address");
    tokio::spawn(async move {
        let _served = axum::serve(listener, router).await;
    });

    (format!("http://{address}"), held)
}

/// Answers `GET /v1/subscriptions?customer=…`, as reconciliation reads it.
async fn list_subscriptions(
    State(held): State<Subscriptions>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let customer = query.get("customer").cloned().unwrap_or_default();
    let data: Vec<Value> = unlock(&held)
        .values()
        .filter(|subscription| subscription["customer"] == customer)
        .cloned()
        .collect();

    Json(json!({ "object": "list", "data": data }))
}

/// Answers `GET /v1/subscriptions/{id}`, refusing an unknown id as Stripe does.
async fn retrieve_subscription(
    State(held): State<Subscriptions>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(subscription) = unlock(&held).get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": {
                "message": format!("No such subscription: '{id}'"),
                "code": "resource_missing",
            } })),
        );
    };

    (StatusCode::OK, Json(subscription))
}

/// One Stripe subscription, in the shape Stripe answers with.
fn subscription(id: &str, customer: &str, status: &str, price: &str) -> Value {
    json!({
        "id": id,
        "object": "subscription",
        "status": status,
        "customer": customer,
        "cancel_at_period_end": false,
        "current_period_end": (Utc::now() + chrono::Duration::days(30)).timestamp(),
        "items": { "object": "list", "data": [{
            "id": "si_test",
            "quantity": 1,
            "price": { "id": price, "recurring": { "interval": "month" } },
        }] },
    })
}

/// The same, carrying the metadata a checkout of ours puts on it.
fn subscription_for(
    id: &str,
    customer: &str,
    status: &str,
    price: &str,
    organization_id: Uuid,
    plan_key: &str,
) -> Value {
    let mut subscription = subscription(id, customer, status, price);
    subscription["metadata"] = json!({
        "organization_id": organization_id.to_string(),
        "plan_key": plan_key,
    });
    subscription
}

/// One Stripe event envelope around `object`.
fn event(id: &str, event_type: &str, created: i64, object: &Value) -> Value {
    json!({
        "id": id,
        "object": "event",
        "type": event_type,
        "created": created,
        "data": { "object": object },
    })
}

/// Posts one event to the receiver, signed the way Stripe signs it.
///
/// The signature is built here rather than by the code under test, so what is
/// proven is the verification, not a round trip through one implementation.
async fn deliver(router: &Router, secret: &str, event: &Value) -> (StatusCode, Value) {
    let body = serde_json::to_string(event).expect("the event must serialize");
    let timestamp = Utc::now().timestamp();
    let mut message = format!("{timestamp}.").into_bytes();
    message.extend_from_slice(body.as_bytes());
    let header = format!(
        "t={timestamp},v1={}",
        signature::sign_hmac_sha256(secret, &message)
    );

    post(router, &header, body).await
}

/// Posts one body with whatever signature header the narrative wants.
async fn post(router: &Router, signature_header: &str, body: String) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(RECEIVER)
        .header("content-type", "application/json")
        .header(signature::STRIPE_SIGNATURE_HEADER, signature_header)
        .body(Body::from(body))
        .expect("the request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the receiver must answer");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body must collect")
        .to_bytes();

    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Runs every stored event that has not been processed, oldest first.
///
/// What the worker would do, without the timing a worker brings.
async fn process_stored_events(reconciler: &Reconciler, connection: &mut AsyncPgConnection) {
    let waiting: Vec<Uuid> = stripe_billing_events::table
        .filter(stripe_billing_events::processed_at.is_null())
        .order(stripe_billing_events::received_at.asc())
        .select(stripe_billing_events::id)
        .load(connection)
        .await
        .expect("the backlog must be readable");

    for event_id in waiting {
        reconciler
            .process(ProcessStripeEvent { event_id })
            .await
            .expect("the event must process");
    }
}

/// The application's config, pointed at the mock Stripe.
fn config(api_base: &str, webhook_secret: Option<&str>) -> AppConfig {
    let api_base = api_base.to_owned();
    let webhook_secret = webhook_secret.map(str::to_owned);
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("https://app.example.com".to_owned()),
        "STRIPE_SECRET_KEY" => Some("sk_test_billing_webhooks".to_owned()),
        "STRIPE_API_BASE" => Some(api_base.clone()),
        "STRIPE_WEBHOOK_SECRET" => webhook_secret.clone(),
        _other => None,
    })
    .expect("the test config must parse")
}

fn plans() -> PlanSet {
    PlanSet::from_yaml(BILLING_YML).expect("plans must parse")
}

/// The receiver and the billing endpoints, mounted where an application mounts
/// them, plus the reconciler its worker would run.
fn mounted(pool: anubis::db::DbPool, config: &AppConfig) -> (Router, Reconciler) {
    let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let router = Router::new()
        .nest(
            "/billing",
            anubis::billing::router(pool.clone(), roles, plans(), config),
        )
        .nest(
            "/webhooks",
            anubis::billing::webhook_router(pool.clone(), config),
        );

    (router, Reconciler::new(pool, plans(), config))
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

/// Names the Stripe customer an organization is billed as, as checkout does.
async fn store_customer(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    customer_id: &str,
) {
    diesel::update(organizations::table.find(organization_id))
        .set(organizations::stripe_customer_id.eq(customer_id))
        .execute(connection)
        .await
        .expect("the customer must store");
}

/// The organization's subscription rows, newest first.
async fn rows_for(connection: &mut AsyncPgConnection, organization_id: Uuid) -> Vec<Value> {
    use anubis::schema::subscriptions;

    subscriptions::table
        .filter(subscriptions::organization_id.eq(organization_id))
        .order(subscriptions::created_at.desc())
        .select((
            subscriptions::stripe_subscription_id,
            subscriptions::plan_key,
            subscriptions::status,
            subscriptions::billing_interval,
        ))
        .load::<(String, String, String, String)>(connection)
        .await
        .expect("the subscriptions must be readable")
        .into_iter()
        .map(|(id, plan_key, status, interval)| {
            json!({ "id": id, "plan": plan_key, "status": status, "interval": interval })
        })
        .collect()
}

/// The organization's one live subscription, as the endpoints resolve it.
async fn live_subscription(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Option<anubis::billing::Subscription> {
    anubis::billing::Subscription::current_for_organization(connection, organization_id)
        .await
        .expect("the query must run")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear narrative from a completed checkout to a second subscription"
)]
async fn stripes_events_write_the_subscription_and_keep_it_current() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, reconciler) = mounted(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let mut connection = harness.pool.get().await.expect("a connection");
    store_customer(&mut connection, organization_id, "cus_lifecycle").await;

    let now = Utc::now().timestamp();

    // 1. The purchase completes. The event names the session and nothing else
    //    worth trusting; what is written is read back from Stripe.
    unlock(&held).insert(
        "sub_lifecycle".to_owned(),
        subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "active",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );
    let completed = event(
        "evt_completed",
        "checkout.session.completed",
        now,
        &json!({
            "id": "cs_test_lifecycle",
            "object": "checkout.session",
            "mode": "subscription",
            "customer": "cus_lifecycle",
            "subscription": "sub_lifecycle",
            "metadata": {
                "organization_id": organization_id.to_string(),
                "plan_key": "pro",
            },
        }),
    );

    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &completed).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["id"].is_string(), "the row's id comes back: {body}");

    // The endpoint stores and queues, and decides nothing itself.
    let queued: i64 = jobs::table
        .filter(jobs::kind.eq("anubis.billing.stripe_event"))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the queue must be readable");
    assert_eq!(queued, 1, "storing an event queues exactly one job");
    assert!(
        live_subscription(&mut connection, organization_id)
            .await
            .is_none(),
        "nothing is decided inside the request",
    );

    process_stored_events(&reconciler, &mut connection).await;
    let live = live_subscription(&mut connection, organization_id)
        .await
        .expect("the completed checkout must write a subscription");
    assert_eq!(live.stripe_subscription_id, "sub_lifecycle");
    assert_eq!(live.plan_key, "pro");
    assert_eq!(live.status, "active");
    assert_eq!(live.billing_interval, "monthly");

    // The billing endpoint now renders the plan that was bought.
    let (status, _headers, body) = send(
        &billing,
        "GET",
        &format!("/billing/organizations/{organization_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "pro", "body: {body}");
    assert_eq!(body["plan"]["limits"]["seats"]["count"], 25, "body: {body}");

    // 2. The customer changes plan in the portal. The event's own body is
    //    deliberately stale here, and what lands is what Stripe answers with.
    unlock(&held).insert(
        "sub_lifecycle".to_owned(),
        subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "past_due",
            "price_scale_monthly",
            organization_id,
            "pro",
        ),
    );
    let updated = event(
        "evt_updated",
        "customer.subscription.updated",
        now + 60,
        &subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "active",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &updated).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    process_stored_events(&reconciler, &mut connection).await;

    let live = live_subscription(&mut connection, organization_id)
        .await
        .expect("a past_due subscription is still live");
    assert_eq!(
        live.plan_key, "scale",
        "the price Stripe reports names the plan, not the event's metadata",
    );
    assert_eq!(
        live.status, "past_due",
        "the state read back from Stripe wins over the event's own snapshot",
    );

    // 3. The subscription ends. A deletion is trusted as it arrives, so the row
    //    is terminal even though the mock still answers `past_due`.
    let deleted = event(
        "evt_deleted",
        "customer.subscription.deleted",
        now + 120,
        &subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "canceled",
            "price_scale_monthly",
            organization_id,
            "scale",
        ),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &deleted).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    process_stored_events(&reconciler, &mut connection).await;

    assert!(
        live_subscription(&mut connection, organization_id)
            .await
            .is_none(),
        "a cancelled subscription is not the current one",
    );

    // 4. A late event from before the cancellation changes nothing.
    unlock(&held).insert(
        "sub_lifecycle".to_owned(),
        subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "active",
            "price_scale_monthly",
            organization_id,
            "scale",
        ),
    );
    let late = event(
        "evt_late",
        "customer.subscription.updated",
        now + 90,
        &subscription_for(
            "sub_lifecycle",
            "cus_lifecycle",
            "active",
            "price_scale_monthly",
            organization_id,
            "scale",
        ),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &late).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    process_stored_events(&reconciler, &mut connection).await;

    assert!(
        live_subscription(&mut connection, organization_id)
            .await
            .is_none(),
        "an event older than the one already applied must not revive a cancellation",
    );

    // 5. The organization subscribes again. A new subscription id is a new row,
    //    and the one-live-subscription index is satisfied because the first is
    //    over.
    unlock(&held).insert(
        "sub_second".to_owned(),
        subscription_for(
            "sub_second",
            "cus_lifecycle",
            "trialing",
            "price_pro_yearly",
            organization_id,
            "pro",
        ),
    );
    let resubscribed = event(
        "evt_resubscribed",
        "customer.subscription.created",
        now + 300,
        &subscription_for(
            "sub_second",
            "cus_lifecycle",
            "trialing",
            "price_pro_yearly",
            organization_id,
            "pro",
        ),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &resubscribed).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    process_stored_events(&reconciler, &mut connection).await;

    let rows = rows_for(&mut connection, organization_id).await;
    assert_eq!(rows.len(), 2, "the history is kept: {rows:?}");
    let live = live_subscription(&mut connection, organization_id)
        .await
        .expect("the new subscription is live");
    assert_eq!(live.stripe_subscription_id, "sub_second");
    assert_eq!(live.billing_interval, "yearly");
    assert_eq!(live.plan_key, "pro");
}

#[tokio::test]
async fn an_event_that_does_not_verify_is_refused_and_stored_nowhere() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, _held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, _reconciler) = mounted(harness.pool.clone(), &config);

    let announced = event(
        "evt_forged",
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription("sub_forged", "cus_forged", "active", "price_pro_monthly"),
    );
    let body = serde_json::to_string(&announced).expect("the event must serialize");
    let timestamp = Utc::now().timestamp();
    let mut message = format!("{timestamp}.").into_bytes();
    message.extend_from_slice(body.as_bytes());
    let signed = signature::sign_hmac_sha256(WEBHOOK_SECRET, &message);

    for (header, reason) in [
        (String::new(), "no signature at all"),
        (format!("t={timestamp}"), "a timestamp and nothing else"),
        (
            format!("t={timestamp},v1={}", "0".repeat(64)),
            "a signature of the right shape and the wrong value",
        ),
        (
            // Signed correctly, but for a timestamp outside the window: a
            // captured request cannot be replayed tomorrow.
            {
                let stale = timestamp - 3_600;
                let mut message = format!("{stale}.").into_bytes();
                message.extend_from_slice(body.as_bytes());
                format!(
                    "t={stale},v1={}",
                    signature::sign_hmac_sha256(WEBHOOK_SECRET, &message)
                )
            },
            "a replay from an hour ago",
        ),
    ] {
        let (status, answered) = post(&billing, &header, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {reason}: {answered}");
    }

    // A signature covers the body, not just the timestamp, so it does not carry
    // to an event that was edited on the way in.
    let tampered = body.replace("active", "past_due");
    assert_ne!(tampered, body, "the fixture must actually change");
    let (status, answered) = post(&billing, &format!("t={timestamp},v1={signed}"), tampered).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {answered}");

    // A body that verifies but is not an event is refused too, because there is
    // nothing to be idempotent about without an id.
    let timestamp = Utc::now().timestamp();
    let nonsense = "{\"hello\":\"world\"}".to_owned();
    let mut message = format!("{timestamp}.").into_bytes();
    message.extend_from_slice(nonsense.as_bytes());
    let header = format!(
        "t={timestamp},v1={}",
        signature::sign_hmac_sha256(WEBHOOK_SECRET, &message)
    );
    let (status, answered) = post(&billing, &header, nonsense).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {answered}");

    let mut connection = harness.pool.get().await.expect("a connection");
    let stored: i64 = stripe_billing_events::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the table must be readable");
    assert_eq!(
        stored, 0,
        "the receiver stores what it verified and nothing else",
    );
}

#[tokio::test]
async fn without_a_signing_secret_the_receiver_says_so() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, _held) = start_mock_stripe().await;
    // Billing is enabled, the receiver is not.
    let config = config(&stripe_base, None);
    let (billing, _reconciler) = mounted(harness.pool.clone(), &config);

    let announced = event(
        "evt_unconfigured",
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription("sub_x", "cus_x", "active", "price_pro_monthly"),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &announced).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("STRIPE_WEBHOOK_SECRET")),
        "the answer names the variable to set, and Stripe's delivery log shows it: {body}",
    );

    let mut connection = harness.pool.get().await.expect("a connection");
    let stored: i64 = stripe_billing_events::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the table must be readable");
    assert_eq!(stored, 0, "an event nobody could verify is not kept");
}

#[tokio::test]
async fn a_redelivered_event_is_stored_once_and_acted_on_once() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, reconciler) = mounted(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let mut connection = harness.pool.get().await.expect("a connection");
    store_customer(&mut connection, organization_id, "cus_redelivered").await;

    unlock(&held).insert(
        "sub_redelivered".to_owned(),
        subscription_for(
            "sub_redelivered",
            "cus_redelivered",
            "active",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );
    let created = event(
        "evt_redelivered",
        "customer.subscription.created",
        Utc::now().timestamp(),
        &subscription_for(
            "sub_redelivered",
            "cus_redelivered",
            "active",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );

    // Stripe redelivers an event it did not hear a 2xx for. Both answer 200, and
    // the second one is the same row: nothing downstream runs twice.
    let (first_status, first) = deliver(&billing, WEBHOOK_SECRET, &created).await;
    let (second_status, second) = deliver(&billing, WEBHOOK_SECRET, &created).await;
    assert_eq!(first_status, StatusCode::OK, "body: {first}");
    assert_eq!(second_status, StatusCode::OK, "body: {second}");
    assert_eq!(first["id"], second["id"], "a redelivery is the same row");

    let stored: i64 = stripe_billing_events::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the table must be readable");
    assert_eq!(stored, 1, "one event, one row");
    let queued: i64 = jobs::table
        .filter(jobs::kind.eq("anubis.billing.stripe_event"))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the queue must be readable");
    assert_eq!(queued, 1, "one event, one job");

    // And the job itself is idempotent: at-least-once delivery means running it
    // twice has to be as good as running it once.
    let event_id: Uuid = stripe_billing_events::table
        .select(stripe_billing_events::id)
        .first(&mut connection)
        .await
        .expect("the stored event must be readable");
    for _attempt in 0..2 {
        reconciler
            .process(ProcessStripeEvent { event_id })
            .await
            .expect("the event must process");
    }

    let rows = rows_for(&mut connection, organization_id).await;
    assert_eq!(rows.len(), 1, "one subscription, not two: {rows:?}");

    // An event of a type the framework does not act on is stored, stamped, and
    // ignored, which is what makes "send all events" a safe way to configure the
    // endpoint in Stripe.
    let unknown = event(
        "evt_invoice",
        "invoice.payment_succeeded",
        Utc::now().timestamp(),
        &json!({ "id": "in_test", "object": "invoice" }),
    );
    let (status, body) = deliver(&billing, WEBHOOK_SECRET, &unknown).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    process_stored_events(&reconciler, &mut connection).await;

    let unprocessed: i64 = stripe_billing_events::table
        .filter(stripe_billing_events::processed_at.is_null())
        .count()
        .get_result(&mut connection)
        .await
        .expect("the table must be readable");
    assert_eq!(unprocessed, 0, "an ignored event is still a processed one");
    assert_eq!(
        rows_for(&mut connection, organization_id).await.len(),
        1,
        "and it changes nothing",
    );
}

#[tokio::test]
async fn reconciling_corrects_a_row_that_drifted_from_stripe() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, reconciler) = mounted(harness.pool.clone(), &config);

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let path = format!("/billing/organizations/{organization_id}");
    let mut connection = harness.pool.get().await.expect("a connection");

    // Before the first purchase there is nothing at Stripe to read.
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/reconcile"),
        Some(&json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert!(body["subscription"].is_null(), "body: {body}");

    // The subscription is bought, and its event never arrives: the endpoint was
    // misconfigured, or this deployment was down while Stripe gave up.
    store_customer(&mut connection, organization_id, "cus_drifted").await;
    unlock(&held).insert(
        "sub_drifted".to_owned(),
        subscription_for(
            "sub_drifted",
            "cus_drifted",
            "active",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );

    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/reconcile"),
        Some(&json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "pro", "body: {body}");
    assert_eq!(body["subscription"]["status"], "active", "body: {body}");

    // Then it is cancelled at Stripe, and again nothing is delivered. The same
    // call the other way corrects it.
    unlock(&held).insert(
        "sub_drifted".to_owned(),
        subscription_for(
            "sub_drifted",
            "cus_drifted",
            "canceled",
            "price_pro_monthly",
            organization_id,
            "pro",
        ),
    );
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &format!("{path}/reconcile"),
        Some(&json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert!(
        body["subscription"].is_null(),
        "a cancelled subscription is not the current one: {body}",
    );

    // The same convergence is available to code, which is what a sweep would
    // call, and it is idempotent.
    assert!(
        reconciler
            .reconcile(organization_id)
            .await
            .expect("reconciliation must run")
            .is_none(),
    );
    let rows = rows_for(&mut connection, organization_id).await;
    assert_eq!(rows.len(), 1, "converging twice writes one row: {rows:?}");
    assert_eq!(rows[0]["status"], "canceled", "rows: {rows:?}");
}

/// Repairing the projection takes the authority that spends the money.
#[tokio::test]
async fn only_members_who_may_spend_money_may_reconcile() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, _held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, _reconciler) = mounted(harness.pool.clone(), &config);
    let mut connection = harness.pool.get().await.expect("a connection");

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let reconcile = format!("/billing/organizations/{organization_id}/reconcile");

    // A member of the organization who holds neither admin nor billing.
    let plain_email = format!("plain-{}@example.com", Uuid::new_v4());
    let plain = register(&harness.router, &plain_email).await;
    let plain_id: Uuid = anubis::schema::users::table
        .filter(anubis::schema::users::email.eq(&plain_email))
        .select(anubis::schema::users::id)
        .first(&mut connection)
        .await
        .expect("the account must exist");
    diesel::insert_into(organization_memberships::table)
        .values((
            organization_memberships::organization_id.eq(organization_id),
            organization_memberships::user_id.eq(plain_id),
            organization_memberships::roles.eq(vec!["default".to_owned()]),
        ))
        .execute(&mut connection)
        .await
        .expect("the membership must insert");

    let (status, _headers, body) =
        send(&billing, "POST", &reconcile, Some(&json!({})), Some(&plain)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    // Somebody else's organization does not exist as far as a stranger is
    // concerned, here as everywhere.
    let stranger = register(
        &harness.router,
        &format!("stranger-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let (status, _headers, body) = send(
        &billing,
        "POST",
        &reconcile,
        Some(&json!({})),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");

    let (status, _headers, body) = send(&billing, "POST", &reconcile, Some(&json!({})), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

/// Two organizations, two customers, and no leak between them.
#[tokio::test]
async fn an_event_reaches_only_the_organization_it_names() {
    let Some(database) = TestDatabase::create("billing_webhooks_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let (stripe_base, held) = start_mock_stripe().await;
    let config = config(&stripe_base, Some(WEBHOOK_SECRET));
    let (billing, reconciler) = mounted(harness.pool.clone(), &config);
    let mut connection = harness.pool.get().await.expect("a connection");

    let first = register(
        &harness.router,
        &format!("first-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let first_organization = bootstrapped_organization(&harness.router, &first).await;
    store_customer(&mut connection, first_organization, "cus_first").await;

    let second = register(
        &harness.router,
        &format!("second-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let second_organization = bootstrapped_organization(&harness.router, &second).await;
    store_customer(&mut connection, second_organization, "cus_second").await;

    let now = Utc::now().timestamp();
    unlock(&held).insert(
        "sub_first".to_owned(),
        subscription_for(
            "sub_first",
            "cus_first",
            "active",
            "price_pro_monthly",
            first_organization,
            "pro",
        ),
    );
    // The second organization's subscription carries no metadata at all, the
    // way one created in the Stripe dashboard would. Its customer is the
    // mapping this application stored itself.
    unlock(&held).insert(
        "sub_second".to_owned(),
        subscription(
            "sub_second",
            "cus_second",
            "trialing",
            "price_scale_monthly",
        ),
    );

    for (id, event_id) in [("sub_first", "evt_first"), ("sub_second", "evt_second")] {
        let object = unlock(&held)
            .get(id)
            .cloned()
            .expect("the mock holds the subscription");
        let announced = event(event_id, "customer.subscription.created", now, &object);
        let (status, body) = deliver(&billing, WEBHOOK_SECRET, &announced).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }
    process_stored_events(&reconciler, &mut connection).await;

    let first_rows = rows_for(&mut connection, first_organization).await;
    assert_eq!(first_rows.len(), 1, "rows: {first_rows:?}");
    assert_eq!(first_rows[0]["id"], "sub_first", "rows: {first_rows:?}");
    assert_eq!(first_rows[0]["plan"], "pro", "rows: {first_rows:?}");

    let second_rows = rows_for(&mut connection, second_organization).await;
    assert_eq!(second_rows.len(), 1, "rows: {second_rows:?}");
    assert_eq!(
        second_rows[0]["id"], "sub_second",
        "the customer resolves the organization when metadata cannot: {second_rows:?}",
    );
    assert_eq!(second_rows[0]["plan"], "scale", "rows: {second_rows:?}");
}
