//! Many subscriptions per organization, each with its line items, end to end.
//!
//! An application that sells one subscription per record (a server, a
//! workspace) holds several live subscriptions for one organization beside its
//! plan subscription, and adds or removes add-ons on them mid-period. This
//! suite drives that through the framework's real paths: signed events into
//! the receiver, the job that processes them, [`Subscriptions`] for the
//! application's own changes, and reconciliation.
//!
//! Stripe is a stateful mock: it applies updates, cancels, keeps an
//! idempotency cache the way Stripe does, pages its lists, and sends only the
//! first page of a long item list, so every behavior asserted here is one the
//! code under test had to earn against Stripe's own shapes. Two of its routes
//! can be held open from the test, which is how the races below are made to
//! actually run rather than happen to run one after the other.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes.

mod support;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anubis::audit::Context;
use anubis::billing::{
    Cancellation, ItemChange, LineItem, NewSubscription, PlanSet, ProcessStripeEvent, Reconciler,
    Subject, Subscription, SubscriptionWithItems, Subscriptions,
};
use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::schema::{audit_events, organizations, stripe_billing_events, subscriptions};
use anubis::webhooks::signature;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use support::{Harness, TestDatabase, register, send};
use tokio::sync::Notify;
use tower::ServiceExt as _;
use uuid::Uuid;

/// The starter's role vocabulary, which is what bootstrapping needs.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [default, billing]
    models:
      Team: [manage]
";

/// The plans. The record prices below are the application's own, in no plan.
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
    limits:
      seats: 25
";

const WEBHOOK_SECRET: &str = "whsec_billing_subscriptions_flow";
const RECEIVER: &str = "/webhooks/stripe-billing";

const SMALL: &str = "price_size_small";
const LARGE: &str = "price_size_large";
const BACKUPS: &str = "price_backups";
const LOGS: &str = "price_logs";

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// One request the mock took, as it arrived.
#[derive(Debug, Clone)]
struct Received {
    method: &'static str,
    path: String,
    body: String,
    idempotency_key: Option<String>,
}

/// Everything the mock Stripe holds.
#[derive(Default)]
struct Ledger {
    /// Subscriptions by id, each with its whole item list.
    subscriptions: BTreeMap<String, Value>,
    requests: Vec<Received>,
    /// Stripe's idempotency cache: a key seen before answers what it answered.
    answered: BTreeMap<String, Value>,
    next_item: u32,
    /// Subscriptions per page of the list; Stripe's own is up to a hundred.
    page_size: usize,
    /// Items embedded in a subscription before its list says `has_more`.
    embedded_items: usize,
}

/// A held request: it signals the first when it arrives, and answers once the
/// second is notified.
type Gate = (Arc<Notify>, Arc<Notify>);

/// The mock, plus the gates a test can hold its routes open with.
#[derive(Clone)]
struct Mock {
    ledger: Arc<Mutex<Ledger>>,
    /// Gates for the next updates, in order: an update applies at once, then
    /// holds its answer at its gate.
    update_gates: Arc<Mutex<VecDeque<Gate>>>,
    /// The same for the next list request.
    list_gate: Arc<Mutex<Option<Gate>>>,
}

impl Mock {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        unlock(&self.ledger)
    }

    /// Holds the next update open: returns (arrived, release).
    fn hold_next_update(&self) -> Gate {
        let gate = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        unlock(&self.update_gates).push_back(gate.clone());
        gate
    }

    /// Holds the next list request open: returns (arrived, release).
    fn hold_next_list(&self) -> Gate {
        let gate = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *unlock(&self.list_gate) = Some(gate.clone());
        gate
    }

    /// Puts a subscription at Stripe, its items given as (price, quantity).
    fn hold(
        &self,
        id: &str,
        customer: &str,
        status: &str,
        metadata: &Value,
        items: &[(&str, i64)],
    ) {
        let mut ledger = self.ledger();
        let mut data = Vec::new();
        for (price, quantity) in items {
            ledger.next_item += 1;
            data.push(item(&format!("si_{}", ledger.next_item), price, *quantity));
        }
        ledger.subscriptions.insert(
            id.to_owned(),
            json!({
                "id": id, "object": "subscription", "status": status, "customer": customer,
                "cancel_at_period_end": false, "metadata": metadata,
                "items": { "object": "list", "data": data, "has_more": false },
            }),
        );
    }

    /// Stripe's whole record of one subscription, as it stands.
    fn snapshot(&self, id: &str) -> Value {
        self.ledger().subscriptions[id].clone()
    }

    /// Sets how Stripe pages: subscriptions per list page, and items embedded
    /// in a subscription before its item list says there are more.
    fn page(&self, page_size: usize, embedded_items: usize) {
        let mut ledger = self.ledger();
        ledger.page_size = page_size;
        ledger.embedded_items = embedded_items;
    }

    /// Adds a line item at Stripe, the way a change made there directly would.
    fn bill(&self, id: &str, price: &str, quantity: i64) {
        let mut ledger = self.ledger();
        ledger.next_item += 1;
        let added = item(&format!("si_{}", ledger.next_item), price, quantity);
        ledger
            .subscriptions
            .get_mut(id)
            .expect("the subscription is held")["items"]["data"]
            .as_array_mut()
            .expect("items are a list")
            .push(added);
    }

    /// Removes a line item at Stripe.
    fn stop_billing(&self, id: &str, price: &str) {
        self.ledger()
            .subscriptions
            .get_mut(id)
            .expect("the subscription is held")["items"]["data"]
            .as_array_mut()
            .expect("items are a list")
            .retain(|item| item["price"]["id"] != json!(price));
    }

    /// Sets a subscription's status at Stripe.
    fn set_status(&self, id: &str, status: &str) {
        self.ledger()
            .subscriptions
            .get_mut(id)
            .expect("the subscription is held")["status"] = json!(status);
    }

    /// The prices Stripe bills a subscription for, with their quantities.
    fn billed(&self, id: &str) -> Vec<(String, i64)> {
        let ledger = self.ledger();
        let subscription = &ledger.subscriptions[id];
        billed_items(subscription)
    }

    /// How many times one subscription was read back.
    fn reads_of(&self, id: &str) -> usize {
        let path = format!("/v1/subscriptions/{id}");
        self.ledger()
            .requests
            .iter()
            .filter(|received| received.method == "GET" && received.path == path)
            .count()
    }

    /// The requests that were writes to subscriptions.
    fn subscription_writes(&self) -> Vec<Received> {
        self.ledger()
            .requests
            .iter()
            .filter(|received| {
                received.method != "GET" && received.path.starts_with("/v1/subscriptions/")
            })
            .cloned()
            .collect()
    }
}

/// One line item in Stripe's shape.
fn item(id: &str, price: &str, quantity: i64) -> Value {
    json!({
        "id": id, "object": "subscription_item", "quantity": quantity,
        "current_period_end": (Utc::now() + chrono::Duration::days(30)).timestamp(),
        "price": { "id": price, "recurring": { "interval": "month" } },
    })
}

fn billed_items(subscription: &Value) -> Vec<(String, i64)> {
    subscription["items"]["data"]
        .as_array()
        .expect("items are a list")
        .iter()
        .map(|item| {
            (
                item["price"]["id"].as_str().unwrap_or_default().to_owned(),
                item["quantity"].as_i64().unwrap_or_default(),
            )
        })
        .collect()
}

/// A subscription as Stripe sends it: the first page of its items only.
fn embed(ledger: &Ledger, subscription: &Value) -> Value {
    let mut sent = subscription.clone();
    let items = subscription["items"]["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if items.len() > ledger.embedded_items {
        sent["items"]["data"] = json!(items[..ledger.embedded_items]);
        sent["items"]["has_more"] = json!(true);
    }
    sent
}

fn refused(message: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "message": message, "code": "parameter_invalid" } })),
    )
}

fn log_request(mock: &Mock, method: &'static str, path: String, headers: &HeaderMap, body: &str) {
    mock.ledger().requests.push(Received {
        method,
        path,
        body: body.to_owned(),
        idempotency_key: headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
}

async fn retrieve(
    State(mock): State<Mock>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    log_request(
        &mock,
        "GET",
        format!("/v1/subscriptions/{id}"),
        &headers,
        "",
    );
    let ledger = mock.ledger();
    let Some(subscription) = ledger.subscriptions.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(
                json!({ "error": { "message": "No such subscription", "code": "resource_missing" } }),
            ),
        );
    };
    (StatusCode::OK, Json(embed(&ledger, subscription)))
}

/// Applies an update the way Stripe does, keyed by its idempotency key.
async fn update(
    State(mock): State<Mock>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Json<Value>) {
    log_request(
        &mock,
        "POST",
        format!("/v1/subscriptions/{id}"),
        &headers,
        &body,
    );
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let answer = {
        let mut ledger = mock.ledger();
        if let Some(cached) = key.as_ref().and_then(|key| ledger.answered.get(key)) {
            return (StatusCode::OK, Json(cached.clone()));
        }
        let mut subscription = ledger.subscriptions[&id].clone();

        let mut changes: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
        for (name, value) in url::form_urlencoded::parse(body.as_bytes()) {
            if name == "cancel_at_period_end" {
                subscription["cancel_at_period_end"] = json!(value == "true");
                continue;
            }
            let Some(rest) = name.strip_prefix("items[") else {
                return refused(&format!("Received unknown parameter: {name}"));
            };
            let (index, field) = rest.split_once("][").expect("items[n][field]");
            changes
                .entry(index.parse().expect("an item index"))
                .or_default()
                .insert(field.trim_end_matches(']').to_owned(), value.into_owned());
        }

        let mut items = subscription["items"]["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for change in changes.values() {
            match change.get("id") {
                Some(item_id) if change.get("deleted").is_some_and(|value| value == "true") => {
                    items.retain(|item| item["id"] != json!(item_id));
                }
                Some(item_id) => {
                    let quantity: i64 = change["quantity"].parse().expect("a quantity");
                    for item in &mut items {
                        if item["id"] == json!(item_id) {
                            item["quantity"] = json!(quantity);
                        }
                    }
                }
                None => {
                    let price = &change["price"];
                    if items.iter().any(|item| item["price"]["id"] == json!(price)) {
                        // Stripe bills a price once per subscription.
                        return refused(
                            "Cannot add multiple subscription items with the same price",
                        );
                    }
                    ledger.next_item += 1;
                    let quantity: i64 = change["quantity"].parse().expect("a quantity");
                    items.push(item(&format!("si_{}", ledger.next_item), price, quantity));
                }
            }
        }
        subscription["items"]["data"] = json!(items);
        ledger
            .subscriptions
            .insert(id.clone(), subscription.clone());

        let answer = embed(&ledger, &subscription);
        if let Some(key) = key {
            ledger.answered.insert(key, answer.clone());
        }
        answer
    };

    let gate = unlock(&mock.update_gates).pop_front();
    if let Some((arrived, release)) = gate {
        arrived.notify_one();
        release.notified().await;
    }
    (StatusCode::OK, Json(answer))
}

async fn cancel(
    State(mock): State<Mock>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    log_request(
        &mock,
        "DELETE",
        format!("/v1/subscriptions/{id}"),
        &headers,
        "",
    );
    let mut ledger = mock.ledger();
    let subscription = ledger
        .subscriptions
        .get_mut(&id)
        .expect("the subscription exists");
    if subscription["status"] == "canceled" {
        return refused("This subscription has already been canceled");
    }
    subscription["status"] = json!("canceled");
    let subscription = subscription.clone();
    (StatusCode::OK, Json(embed(&ledger, &subscription)))
}

/// Pages the customer's subscriptions, `page_size` at a time.
async fn list(
    State(mock): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let page = {
        let ledger = mock.ledger();
        let held: Vec<&Value> = ledger
            .subscriptions
            .values()
            .filter(|subscription| {
                Some(subscription["customer"].as_str().unwrap_or_default())
                    == query.get("customer").map(String::as_str)
            })
            .collect();
        let start = query.get("starting_after").map_or(0, |after| {
            held.iter()
                .position(|subscription| subscription["id"] == json!(after))
                .expect("starting_after names a listed subscription")
                + 1
        });
        let end = (start + ledger.page_size).min(held.len());
        let data: Vec<Value> = held[start..end]
            .iter()
            .map(|subscription| embed(&ledger, subscription))
            .collect();
        json!({ "object": "list", "data": data, "has_more": end < held.len() })
    };

    let gate = unlock(&mock.list_gate).take();
    if let Some((arrived, release)) = gate {
        arrived.notify_one();
        release.notified().await;
    }
    Json(page)
}

/// The whole item list of one subscription.
async fn items(
    State(mock): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let ledger = mock.ledger();
    let subscription = &ledger.subscriptions[&query["subscription"]];
    Json(json!({
        "object": "list",
        "data": subscription["items"]["data"],
        "has_more": false,
    }))
}

async fn checkout(State(mock): State<Mock>, headers: HeaderMap, body: String) -> Json<Value> {
    log_request(
        &mock,
        "POST",
        "/v1/checkout/sessions".to_owned(),
        &headers,
        &body,
    );
    Json(json!({ "id": "cs_test_record", "url": "https://checkout.stripe.test/cs_test_record" }))
}

async fn customer(State(mock): State<Mock>, headers: HeaderMap, body: String) -> Json<Value> {
    log_request(&mock, "POST", "/v1/customers".to_owned(), &headers, &body);
    Json(json!({ "id": "cus_created" }))
}

async fn start_mock_stripe() -> (String, Mock) {
    let mock = Mock {
        ledger: Arc::new(Mutex::new(Ledger {
            page_size: 100,
            embedded_items: 10,
            ..Ledger::default()
        })),
        update_gates: Arc::default(),
        list_gate: Arc::default(),
    };
    let router = Router::new()
        .route("/v1/customers", post(customer))
        .route("/v1/checkout/sessions", post(checkout))
        .route("/v1/subscriptions", get(list))
        .route(
            "/v1/subscriptions/{id}",
            get(retrieve).post(update).delete(cancel),
        )
        .route("/v1/subscription_items", get(items))
        .with_state(mock.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the mock must bind a loopback port");
    let address = listener
        .local_addr()
        .expect("the mock must report its address");
    tokio::spawn(async move {
        let _served = axum::serve(listener, router).await;
    });

    (format!("http://{address}"), mock)
}

fn config(api_base: &str) -> AppConfig {
    let api_base = api_base.to_owned();
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("https://app.example.com".to_owned()),
        "STRIPE_SECRET_KEY" => Some("sk_test_subscriptions_flow".to_owned()),
        "STRIPE_API_BASE" => Some(api_base.clone()),
        "STRIPE_WEBHOOK_SECRET" => Some(WEBHOOK_SECRET.to_owned()),
        _other => None,
    })
    .expect("the test config must parse")
}

fn plans() -> PlanSet {
    PlanSet::from_yaml(BILLING_YML).expect("plans must parse")
}

/// What one narrative needs: the mounted billing surface, the job handler,
/// the application's service, the mock, and an organization with a customer.
struct Scene {
    harness: Harness,
    billing: Router,
    reconciler: Reconciler,
    subscriptions: Subscriptions,
    mock: Mock,
    owner: String,
    organization_id: Uuid,
    customer: String,
}

impl Scene {
    async fn new(database: &TestDatabase) -> Self {
        let harness = Harness::boot(database).await;
        let (stripe_base, mock) = start_mock_stripe().await;
        let config = config(&stripe_base);
        let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
        let billing = Router::new()
            .nest(
                "/billing",
                anubis::billing::router(harness.pool.clone(), roles, plans(), &config),
            )
            .nest(
                "/webhooks",
                anubis::billing::webhook_router(harness.pool.clone(), &config),
            );
        let reconciler = Reconciler::new(harness.pool.clone(), plans(), &config);
        let subscriptions = Subscriptions::new(harness.pool.clone(), plans(), &config);

        let owner = register(
            &harness.router,
            &format!("owner-{}@example.com", Uuid::new_v4()),
        )
        .await;
        let organization_id = organization_of(&harness.router, &owner).await;
        let customer = format!("cus_{}", organization_id.simple());
        let mut connection = harness.pool.get().await.expect("a connection");
        diesel::update(organizations::table.find(organization_id))
            .set(organizations::stripe_customer_id.eq(&customer))
            .execute(&mut connection)
            .await
            .expect("the customer must store");

        Self {
            harness,
            billing,
            reconciler,
            subscriptions,
            mock,
            owner,
            organization_id,
            customer,
        }
    }

    async fn connection(
        &self,
    ) -> diesel_async::pooled_connection::deadpool::Object<AsyncPgConnection> {
        self.harness.pool.get().await.expect("a connection")
    }

    /// The metadata a checkout of a record puts on its subscription.
    fn record_metadata(&self, server: &Subject) -> Value {
        json!({
            "organization_id": self.organization_id.to_string(),
            "subject_type": server.kind(),
            "subject_id": server.id().to_string(),
        })
    }

    /// Delivers one signed event, then runs every stored event that waits.
    async fn deliver(&self, id: &str, event_type: &str, created: i64, object: &Value) {
        let event = json!({
            "id": id, "object": "event", "type": event_type, "created": created,
            "data": { "object": object },
        });
        let (status, body) = deliver(&self.billing, &event).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    /// Announces a subscription the mock holds, the way Stripe's event does.
    async fn announce(&self, subscription_id: &str, created: i64) {
        let object = self.mock.snapshot(subscription_id);
        self.deliver(
            &format!(
                "evt_{subscription_id}_{created}_{}",
                Uuid::new_v4().simple()
            ),
            "customer.subscription.updated",
            created,
            &object,
        )
        .await;
        self.process().await;
    }

    async fn process(&self) {
        for (event_id, outcome) in self.process_all().await {
            outcome.unwrap_or_else(|error| panic!("event {event_id} must process: {error}"));
        }
    }

    /// Runs every unprocessed event, oldest first, reporting each outcome.
    async fn process_all(&self) -> Vec<(Uuid, Result<(), String>)> {
        let mut connection = self.connection().await;
        let waiting: Vec<Uuid> = stripe_billing_events::table
            .filter(stripe_billing_events::processed_at.is_null())
            .order(stripe_billing_events::received_at.asc())
            .select(stripe_billing_events::id)
            .load(&mut connection)
            .await
            .expect("the backlog must be readable");
        let mut outcomes = Vec::new();
        for event_id in waiting {
            let outcome = self
                .reconciler
                .process(ProcessStripeEvent { event_id })
                .await
                .map_err(|error| error.to_string());
            outcomes.push((event_id, outcome));
        }
        outcomes
    }

    /// The live subscription paying for `server`, with its items.
    async fn live(&self, server: &Subject) -> Option<SubscriptionWithItems> {
        let mut connection = self.connection().await;
        let held = Subscription::live_for_subject(&mut connection, self.organization_id, server)
            .await
            .expect("the query must run")?;
        SubscriptionWithItems::load(&mut connection, vec![held])
            .await
            .expect("the items must load")
            .pop()
    }

    async fn billing_body(&self) -> Value {
        let (status, _headers, body) = send(
            &self.billing,
            "GET",
            &format!("/billing/organizations/{}", self.organization_id),
            None,
            Some(&self.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        body
    }

    async fn audited(&self, action: &str) -> Vec<Value> {
        let mut connection = self.connection().await;
        audit_events::table
            .filter(audit_events::organization_id.eq(self.organization_id))
            .filter(audit_events::action.eq(action))
            .order(audit_events::created_at.asc())
            .select(audit_events::changes)
            .load(&mut connection)
            .await
            .expect("the audit log must be readable")
    }
}

/// The stored items of a subscription, as (price, quantity), sorted by price.
fn stored(held: &SubscriptionWithItems) -> Vec<(String, i64)> {
    let mut items: Vec<(String, i64)> = held
        .items
        .iter()
        .map(|item| {
            (
                item.stripe_price_id.clone(),
                i64::from(item.quantity.unwrap_or_default()),
            )
        })
        .collect();
    items.sort();
    items
}

fn pairs(items: &[(&str, i64)]) -> Vec<(String, i64)> {
    let mut pairs: Vec<(String, i64)> = items
        .iter()
        .map(|(price, quantity)| ((*price).to_owned(), *quantity))
        .collect();
    pairs.sort();
    pairs
}

async fn organization_of(router: &Router, cookie: &str) -> Uuid {
    let (status, _headers, body) =
        send(router, "GET", "/tenancy/memberships", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["organizations"][0]["id"]
        .as_str()
        .expect("registration bootstraps one organization")
        .parse()
        .expect("an organization id is a UUID")
}

/// Posts one event to the receiver, signed the way Stripe signs it.
async fn deliver(router: &Router, event: &Value) -> (StatusCode, Value) {
    let body = serde_json::to_string(event).expect("the event must serialize");
    let timestamp = Utc::now().timestamp();
    let mut message = format!("{timestamp}.").into_bytes();
    message.extend_from_slice(body.as_bytes());
    let header = format!(
        "t={timestamp},v1={}",
        signature::sign_hmac_sha256(WEBHOOK_SECRET, &message)
    );
    let request = Request::builder()
        .method("POST")
        .uri(RECEIVER)
        .header("content-type", "application/json")
        .header(signature::STRIPE_SIGNATURE_HEADER, header)
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

fn server() -> Subject {
    Subject::new("Server", Uuid::new_v4())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one narrative: a plan and two records, then each rule about them in turn"
)]
async fn an_organization_holds_its_plan_and_a_subscription_per_record() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let now = Utc::now().timestamp();
    let (first, second) = (server(), server());

    let plan_metadata = json!({
        "organization_id": scene.organization_id.to_string(),
        "plan_key": "pro",
    });
    scene.mock.hold(
        "sub_plan",
        &scene.customer,
        "active",
        &plan_metadata,
        &[("price_pro_monthly", 1)],
    );
    scene.mock.hold(
        "sub_first",
        &scene.customer,
        "active",
        &scene.record_metadata(&first),
        &[(SMALL, 1), (BACKUPS, 1)],
    );
    scene.mock.hold(
        "sub_second",
        &scene.customer,
        "trialing",
        &scene.record_metadata(&second),
        &[(LARGE, 2)],
    );
    for id in ["sub_plan", "sub_first", "sub_second"] {
        scene.announce(id, now).await;
    }

    // Every live subscription is served, each with its line items, and the
    // plan in force is the plan subscription's.
    let body = scene.billing_body().await;
    assert_eq!(body["plan"]["key"], "pro", "body: {body}");
    assert_eq!(body["subscription"]["stripe_subscription_id"], "sub_plan");
    assert_eq!(
        body["subscription"]["items"][0]["stripe_price_id"],
        "price_pro_monthly"
    );
    let listed = body["subscriptions"].as_array().expect("a list");
    assert_eq!(listed.len(), 3, "body: {body}");
    assert_eq!(
        listed[0]["stripe_subscription_id"], "sub_plan",
        "the plan comes first"
    );
    for record in &listed[1..] {
        assert_eq!(record["subject_type"], "Server", "body: {body}");
        assert!(
            record["plan_key"].is_null(),
            "a record buys no plan: {body}"
        );
    }
    let first_held = scene
        .live(&first)
        .await
        .expect("the first record is paid for");
    assert_eq!(stored(&first_held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
    assert_eq!(first_held.subscription.plan_key, None);
    let second_held = scene
        .live(&second)
        .await
        .expect("the second record is paid for");
    assert_eq!(stored(&second_held), pairs(&[(LARGE, 2)]));

    // Limits follow the plan subscription however many records are paid for.
    let limits = anubis::billing::Limits::new(plans());
    let mut connection = scene.connection().await;
    let plan = limits
        .plan_for(&mut connection, scene.organization_id)
        .await
        .expect("the plan must resolve");
    assert_eq!(plan.key(), "pro");
    drop(connection);

    // A second live plan subscription, or a second one for a record already
    // paid for, is a decision a person has to make: the job fails, saying so,
    // and the event keeps the reason.
    scene.mock.hold(
        "sub_plan_again",
        &scene.customer,
        "active",
        &plan_metadata,
        &[("price_pro_monthly", 1)],
    );
    scene.mock.hold(
        "sub_first_again",
        &scene.customer,
        "active",
        &scene.record_metadata(&first),
        &[(SMALL, 1)],
    );
    for id in ["sub_plan_again", "sub_first_again"] {
        let object = scene.mock.snapshot(id);
        scene
            .deliver(
                &format!("evt_{id}"),
                "customer.subscription.created",
                now,
                &object,
            )
            .await;
    }
    let outcomes = scene.process_all().await;
    assert_eq!(outcomes.len(), 2);
    let failures: Vec<String> = outcomes
        .into_iter()
        .map(|(_event, outcome)| {
            outcome.expect_err("a second live subscription for one slot fails")
        })
        .collect();
    assert!(
        failures[0].contains("a live plan subscription"),
        "the failure names the slot: {failures:?}",
    );
    assert!(
        failures[1].contains(&format!("a live subscription for {first}")),
        "the failure names the record: {failures:?}",
    );

    // The plan subscription ends. The records are still paid for, and they do
    // not hold the organization on a paid plan: each buys its own record.
    let mut ended = scene.mock.snapshot("sub_plan");
    ended["status"] = json!("canceled");
    scene
        .deliver(
            "evt_plan_deleted",
            "customer.subscription.deleted",
            now + 60,
            &ended,
        )
        .await;
    let outcomes = scene.process_all().await;
    assert!(
        outcomes.iter().any(|(_event, outcome)| outcome.is_ok()),
        "the deletion processes: {outcomes:?}",
    );

    let body = scene.billing_body().await;
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert!(body["subscription"].is_null(), "body: {body}");
    assert_eq!(
        body["subscriptions"].as_array().map(Vec::len),
        Some(2),
        "both records are still paid for: {body}",
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one narrative from opening a checkout to the event and the refusals around it"
)]
async fn a_record_is_bought_through_checkout_and_arrives_as_an_event() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let line_items = [LineItem::new(SMALL, 1), LineItem::new(BACKUPS, 1)];
    let return_path = format!("/servers/{}", record.id());

    let session = scene
        .subscriptions
        .checkout(
            scene.organization_id,
            NewSubscription {
                subject: &record,
                line_items: &line_items,
                return_path: &return_path,
                email: Some("owner@example.com"),
            },
        )
        .await
        .expect("the checkout must open");
    assert_eq!(session.url, "https://checkout.stripe.test/cs_test_record");

    // One session naming every line item, and the record on the subscription
    // it creates, so every later event identifies what it pays for.
    let sessions: Vec<Received> = scene
        .mock
        .ledger()
        .requests
        .iter()
        .filter(|received| received.path == "/v1/checkout/sessions")
        .cloned()
        .collect();
    assert_eq!(sessions.len(), 1);
    let form: BTreeMap<String, String> = url::form_urlencoded::parse(sessions[0].body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(form["line_items[0][price]"], SMALL);
    assert_eq!(form["line_items[1][price]"], BACKUPS);
    assert_eq!(form["line_items[1][quantity]"], "1");
    assert_eq!(form["subscription_data[metadata][subject_type]"], "Server");
    assert_eq!(
        form["subscription_data[metadata][subject_id]"],
        record.id().to_string()
    );
    assert_eq!(
        form["subscription_data[metadata][organization_id]"],
        scene.organization_id.to_string(),
    );
    assert!(
        !form.contains_key("subscription_data[metadata][plan_key]"),
        "a record buys no plan: {form:?}",
    );
    assert_eq!(
        form["success_url"],
        format!(
            "https://app.example.com{return_path}?checkout=success&session_id={{CHECKOUT_SESSION_ID}}"
        ),
    );
    assert_eq!(
        form["cancel_url"],
        format!("https://app.example.com{return_path}?checkout=canceled")
    );

    // A path that would leave the application, or line items Stripe would
    // refuse, are refused before Stripe hears of them.
    for (path, items) in [
        ("//evil.example.com", line_items.as_slice()),
        (return_path.as_str(), &[] as &[LineItem]),
    ] {
        let refused = scene
            .subscriptions
            .checkout(
                scene.organization_id,
                NewSubscription {
                    subject: &record,
                    line_items: items,
                    return_path: path,
                    email: None,
                },
            )
            .await
            .expect_err("the purchase must be refused");
        assert!(refused.is_invalid(), "{refused}");
    }

    // The customer pays; Stripe's event writes the subscription.
    scene.mock.hold(
        "sub_bought",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1), (BACKUPS, 1)],
    );
    scene
        .deliver(
            "evt_bought",
            "checkout.session.completed",
            Utc::now().timestamp(),
            &json!({ "id": "cs_test_record", "object": "checkout.session",
                     "customer": scene.customer, "subscription": "sub_bought" }),
        )
        .await;
    scene.process().await;
    let held = scene.live(&record).await.expect("the record is paid for");
    assert_eq!(held.subscription.subject(), Some(record.clone()));
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));

    // A record already paid for cannot be bought again, and Stripe is not
    // asked to take a second payment.
    let conflict = scene
        .subscriptions
        .checkout(
            scene.organization_id,
            NewSubscription {
                subject: &record,
                line_items: &line_items,
                return_path: &return_path,
                email: None,
            },
        )
        .await
        .expect_err("a second purchase of one record is refused");
    assert!(conflict.is_conflict(), "{conflict}");
    let sessions_after = scene
        .mock
        .ledger()
        .requests
        .iter()
        .filter(|received| received.path == "/v1/checkout/sessions")
        .count();
    assert_eq!(sessions_after, 1, "no second session was opened");

    // Without a Stripe key nothing can be bought, and the answer says why.
    let unconfigured = AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _other => None,
    })
    .expect("the test config must parse");
    let offline = Subscriptions::new(scene.harness.pool.clone(), plans(), &unconfigured);
    let refused = offline
        .checkout(
            scene.organization_id,
            NewSubscription {
                subject: &server(),
                line_items: &line_items,
                return_path: "/servers",
                email: None,
            },
        )
        .await
        .expect_err("billing is off");
    assert!(refused.is_unconfigured(), "{refused}");
    let answered: anubis::http::ApiError = refused.into();
    assert_eq!(answered.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        answered.message().contains("STRIPE_SECRET_KEY"),
        "{}",
        answered.message()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one narrative from an add-on bought mid-period to its removal and the events after"
)]
async fn add_ons_change_mid_period_and_stay_in_step_with_stripes_events() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let context = Context::system();
    let start = Utc::now().timestamp() - 600;

    scene.mock.hold(
        "sub_server",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene.announce("sub_server", start).await;

    // An add-on bought mid-period: one update naming the new price, keyed, and
    // with nothing about proration, which leaves Stripe's own.
    let held = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("the add-on must be bought");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
    assert_eq!(
        scene.mock.billed("sub_server"),
        vec![(SMALL.to_owned(), 1), (BACKUPS.to_owned(), 1)]
    );
    let writes = scene.mock.subscription_writes();
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0]
            .body
            .contains("items%5B0%5D%5Bprice%5D=price_backups"),
        "{writes:?}"
    );
    assert!(
        !writes[0].body.contains("proration"),
        "Stripe's default proration: {writes:?}"
    );
    assert!(
        writes[0]
            .idempotency_key
            .as_deref()
            .is_some_and(|key| key.starts_with("anubis-subscription-")),
        "every update is keyed: {writes:?}",
    );

    // Asking again is the same as asking once: nothing is sent.
    scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("a repeat is harmless");
    assert_eq!(
        scene.mock.subscription_writes().len(),
        1,
        "a repeat sends nothing"
    );

    // A resize and a swap: an existing item is named by its id.
    let held = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[
                ItemChange::remove(SMALL),
                ItemChange::set(LARGE, 1),
                ItemChange::set(BACKUPS, 2),
            ],
        )
        .await
        .expect("the swap must apply");
    assert_eq!(stored(&held), pairs(&[(LARGE, 1), (BACKUPS, 2)]));
    let writes = scene.mock.subscription_writes();
    assert_eq!(writes.len(), 2, "a swap is one request: {writes:?}");
    assert!(writes[1].body.contains("deleted%5D=true"), "{writes:?}");
    assert_ne!(
        writes[0].idempotency_key, writes[1].idempotency_key,
        "a different change is a different key",
    );

    // The add-on is removed.
    let held = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::remove(BACKUPS)],
        )
        .await
        .expect("the add-on must be removed");
    assert_eq!(stored(&held), pairs(&[(LARGE, 1)]));

    // Every change that moved something is in the audit log, on the
    // organization, with the items before and after.
    let changes = scene
        .audited(anubis::audit::SUBSCRIPTION_ITEMS_CHANGED)
        .await;
    assert_eq!(
        changes.len(),
        3,
        "three changes, the repeat not among them: {changes:?}"
    );
    assert_eq!(
        changes[2]["items"]["new"],
        json!([{ "price": LARGE, "quantity": 1 }])
    );

    // A late event from before those changes, read against a Stripe that
    // still bills the add-on, changes neither the subscription nor its items.
    scene.mock.bill("sub_server", BACKUPS, 1);
    let stale = scene.mock.snapshot("sub_server");
    scene
        .deliver(
            "evt_stale",
            "customer.subscription.updated",
            start + 1,
            &stale,
        )
        .await;
    scene.process().await;
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(
        stored(&held),
        pairs(&[(LARGE, 1)]),
        "an event older than the last write leaves the items alone",
    );

    // A newer event, about a change made at Stripe directly (the dashboard),
    // is read back and wins: the items become what Stripe reports.
    scene.mock.stop_billing("sub_server", BACKUPS);
    scene.mock.bill("sub_server", LOGS, 1);
    let newer = Utc::now().timestamp() + 60;
    scene.announce("sub_server", newer).await;
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(stored(&held), pairs(&[(LARGE, 1), (LOGS, 1)]));

    // Redelivered, the same event is one row and changes nothing twice.
    let object = scene.mock.snapshot("sub_server");
    for _delivery in 0..2 {
        scene
            .deliver(
                "evt_redelivered",
                "customer.subscription.updated",
                newer,
                &object,
            )
            .await;
    }
    scene.process().await;
    let mut connection = scene.connection().await;
    let stored_events: i64 = stripe_billing_events::table
        .filter(stripe_billing_events::stripe_event_id.eq("evt_redelivered"))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the events must count");
    assert_eq!(stored_events, 1);
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(
        stored(&held),
        pairs(&[(LARGE, 1), (LOGS, 1)]),
        "no item is doubled"
    );

    // Removing every item is cancelling, and is refused as a change.
    let refused = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::remove(LARGE), ItemChange::remove(LOGS)],
        )
        .await
        .expect_err("an empty subscription is not a change");
    assert!(refused.is_invalid(), "{refused}");
}

#[tokio::test]
async fn a_record_another_organization_pays_for_cannot_be_changed_from_this_one() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_theirs",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene.announce("sub_theirs", Utc::now().timestamp()).await;

    let stranger = register(
        &scene.harness.router,
        &format!("stranger-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let other_organization = organization_of(&scene.harness.router, &stranger).await;
    let context = Context::system();

    let refused = scene
        .subscriptions
        .change_items(
            &context,
            other_organization,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("another tenant's record is not this one's to change");
    assert!(refused.is_not_found(), "{refused}");
    let answered: anubis::http::ApiError = refused.into();
    assert_eq!(answered.status(), StatusCode::NOT_FOUND);

    let refused = scene
        .subscriptions
        .cancel(&context, other_organization, &record, Cancellation::Now)
        .await
        .expect_err("nor to cancel");
    assert!(refused.is_not_found(), "{refused}");

    assert!(
        scene.mock.subscription_writes().is_empty(),
        "Stripe heard nothing"
    );
    let held = scene
        .live(&record)
        .await
        .expect("the owner's subscription is untouched");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1)]));
}

#[tokio::test]
async fn cancelling_ends_a_subscription_at_period_end_or_now() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let context = Context::system();
    scene.mock.hold(
        "sub_ending",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_ending", Utc::now().timestamp() - 600)
        .await;

    let held = scene
        .subscriptions
        .cancel(
            &context,
            scene.organization_id,
            &record,
            Cancellation::AtPeriodEnd,
        )
        .await
        .expect("the subscription is set to end");
    assert!(held.subscription.cancel_at_period_end);
    assert_eq!(
        held.subscription.status, "active",
        "it runs to the end of the period"
    );
    let writes = scene.mock.subscription_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body, "cancel_at_period_end=true");

    scene
        .subscriptions
        .cancel(
            &context,
            scene.organization_id,
            &record,
            Cancellation::AtPeriodEnd,
        )
        .await
        .expect("asking twice is harmless");
    assert_eq!(
        scene.mock.subscription_writes().len(),
        1,
        "already set, so nothing is sent"
    );

    let held = scene
        .subscriptions
        .cancel(&context, scene.organization_id, &record, Cancellation::Now)
        .await
        .expect("the subscription ends now");
    assert_eq!(held.subscription.status, "canceled");
    assert_eq!(scene.mock.subscription_writes()[1].method, "DELETE");
    assert!(
        scene.live(&record).await.is_none(),
        "nothing pays for the record now"
    );

    let canceled = scene.audited(anubis::audit::SUBSCRIPTION_CANCELED).await;
    assert_eq!(canceled.len(), 2, "{canceled:?}");
    assert_eq!(canceled[0]["cancel_at_period_end"]["new"], true);
    assert_eq!(canceled[1]["status"]["new"], "canceled");

    let refused = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("a cancelled subscription takes no add-ons");
    assert!(refused.is_not_found(), "{refused}");
}

#[tokio::test]
async fn reconciliation_converges_every_subscription_and_its_items() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    // Small pages and a short embedded item list, so both have to be followed
    // to their ends.
    scene.mock.page(2, 2);
    let records: Vec<Subject> = (0..4).map(|_index| server()).collect();
    for (index, record) in records.iter().enumerate() {
        let items: &[(&str, i64)] = if index == 0 {
            &[(SMALL, 1), (BACKUPS, 1), (LOGS, 1)]
        } else {
            &[(LARGE, 1)]
        };
        scene.mock.hold(
            &format!("sub_record_{index}"),
            &scene.customer,
            "active",
            &scene.record_metadata(record),
            items,
        );
    }

    // One row this application already holds, with an item Stripe dropped.
    scene
        .announce("sub_record_1", Utc::now().timestamp() - 600)
        .await;
    scene.mock.stop_billing("sub_record_1", LARGE);
    scene.mock.bill("sub_record_1", SMALL, 3);
    let live = scene
        .reconciler
        .reconcile(scene.organization_id)
        .await
        .expect("reconciliation must run");
    assert_eq!(live.len(), 4, "every page was read: {live:?}");
    let first = scene.live(&records[0]).await.expect("written");
    assert_eq!(
        stored(&first),
        pairs(&[(SMALL, 1), (BACKUPS, 1), (LOGS, 1)]),
        "a long item list is read to its end",
    );
    let drifted = scene.live(&records[1]).await.expect("written");
    assert_eq!(
        stored(&drifted),
        pairs(&[(SMALL, 3)]),
        "the dropped item is gone"
    );

    // One subscription that cannot be recorded is named, and the rest are
    // still repaired rather than held back by it.
    let weekly = server();
    let mut metadata = scene.record_metadata(&weekly);
    metadata["note"] = json!("billed weekly");
    scene.mock.hold(
        "sub_weekly",
        &scene.customer,
        "active",
        &metadata,
        &[(SMALL, 1)],
    );
    scene
        .mock
        .ledger()
        .subscriptions
        .get_mut("sub_weekly")
        .expect("held")["items"]["data"][0]["price"]["recurring"]["interval"] = json!("week");
    // The last record's subscription was replaced at Stripe: the old one is
    // cancelled and a new one pays for the record. Stripe lists both, and the
    // cancelled one has to free the record's slot before the new one takes it.
    scene.mock.set_status("sub_record_3", "canceled");
    scene.mock.hold(
        "sub_repairable",
        &scene.customer,
        "active",
        &scene.record_metadata(&records[3]),
        &[(LOGS, 2)],
    );

    let failure = scene
        .reconciler
        .reconcile(scene.organization_id)
        .await
        .expect_err("one subscription cannot be recorded");
    assert!(failure.message().contains("sub_weekly"), "{failure}");
    let repaired = scene
        .live(&records[3])
        .await
        .expect("the rest were repaired");
    assert_eq!(
        repaired.subscription.stripe_subscription_id,
        "sub_repairable"
    );
    assert!(scene.live(&weekly).await.is_none());

    // The same over HTTP: the screen's refresh answers with every subscription.
    scene.mock.ledger().subscriptions.remove("sub_weekly");
    let (status, _headers, body) = send(
        &scene.billing,
        "POST",
        &format!("/billing/organizations/{}/reconcile", scene.organization_id),
        Some(&json!({})),
        Some(&scene.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["subscriptions"].as_array().map(Vec::len),
        Some(4),
        "body: {body}"
    );
}

/// Two changes to one subscription run one after the other.
///
/// The first change's answer is held at Stripe while the second is started.
/// Were the second free to run, its answer (carrying both add-ons) would be
/// written first and the first one's (carrying one) after it, losing an
/// add-on the customer is paying for. The row lock makes it wait instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_changes_to_one_subscription_are_applied_one_after_the_other() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_busy",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_busy", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_update();
    let first = tokio::spawn({
        let scene = Arc::clone(&scene);
        let record = record.clone();
        async move {
            scene
                .subscriptions
                .change_items(
                    &Context::system(),
                    scene.organization_id,
                    &record,
                    &[ItemChange::set(BACKUPS, 1)],
                )
                .await
        }
    });
    arrived.notified().await;

    let second = tokio::spawn({
        let scene = Arc::clone(&scene);
        let record = record.clone();
        async move {
            scene
                .subscriptions
                .change_items(
                    &Context::system(),
                    scene.organization_id,
                    &record,
                    &[ItemChange::set(LOGS, 1)],
                )
                .await
        }
    });
    // Long enough for the second change to reach Stripe if nothing stops it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        scene.mock.subscription_writes().len(),
        1,
        "the second change waits for the first rather than racing it",
    );
    release.notify_one();

    first
        .await
        .expect("the task must join")
        .expect("the first change applies");
    second
        .await
        .expect("the task must join")
        .expect("the second change applies");

    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1), (LOGS, 1)]));
    assert_eq!(scene.mock.billed("sub_busy").len(), 3);
}

/// Two purchases of one record completing at once leave one live subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_subscriptions_for_one_record_at_once_leave_one_live() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    let now = Utc::now().timestamp();
    for id in ["sub_tab_one", "sub_tab_two"] {
        scene.mock.hold(
            id,
            &scene.customer,
            "active",
            &scene.record_metadata(&record),
            &[(SMALL, 1)],
        );
        let object = scene.mock.snapshot(id);
        scene
            .deliver(
                &format!("evt_{id}"),
                "customer.subscription.created",
                now,
                &object,
            )
            .await;
    }

    let mut connection = scene.connection().await;
    let events: Vec<Uuid> = stripe_billing_events::table
        .select(stripe_billing_events::id)
        .load(&mut connection)
        .await
        .expect("the events must be readable");
    let racers: Vec<_> = events
        .into_iter()
        .map(|event_id| {
            let reconciler = scene.reconciler.clone();
            tokio::spawn(async move { reconciler.process(ProcessStripeEvent { event_id }).await })
        })
        .collect();
    let mut failures = Vec::new();
    for racer in racers {
        if let Err(error) = racer.await.expect("the task must join") {
            failures.push(error.to_string());
        }
    }

    assert_eq!(failures.len(), 1, "exactly one is refused: {failures:?}");
    assert!(failures[0].contains(&record.to_string()), "{failures:?}");
    let live: i64 = subscriptions::table
        .filter(subscriptions::subject_id.eq(record.id()))
        .filter(subscriptions::status.eq("active"))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the rows must count");
    assert_eq!(live, 1);
}

/// A write that read Stripe directly never undoes a newer event.
///
/// Reconciliation reads a subscription, and while its answer is held, the
/// subscription changes at Stripe and that change's event is processed. The
/// reconciliation's write, carrying what it read before, lands last; it is
/// stamped with when its read began, so the event's newer state stands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_holding_an_older_read_does_not_undo_a_newer_event() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_raced",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_raced", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_list();
    let reconciling = tokio::spawn({
        let scene = Arc::clone(&scene);
        async move { scene.reconciler.reconcile(scene.organization_id).await }
    });
    arrived.notified().await;

    // Stripe's clock moves past the read, and the add-on bought then is
    // announced and processed while the reconciliation is still in flight.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    scene.mock.bill("sub_raced", BACKUPS, 1);
    scene.announce("sub_raced", Utc::now().timestamp()).await;
    release.notify_one();
    reconciling
        .await
        .expect("the task must join")
        .expect("reconciliation runs");

    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(
        stored(&held),
        pairs(&[(SMALL, 1), (BACKUPS, 1)]),
        "the older read did not remove the add-on the newer event recorded",
    );
}

/// A cancellation in the same second as a write is never dropped as older.
///
/// Stripe stamps an event in whole seconds. A write stamped with a fraction of
/// a second would sort after a cancellation created later in the same second,
/// and the cancelled subscription would keep granting access.
#[tokio::test]
async fn a_cancellation_in_the_same_second_as_a_write_still_applies() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_brief",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_brief", Utc::now().timestamp() - 600)
        .await;

    scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("the add-on must be bought");

    let mut ended = scene.mock.snapshot("sub_brief");
    ended["status"] = json!("canceled");
    scene
        .deliver(
            "evt_brief_deleted",
            "customer.subscription.deleted",
            Utc::now().timestamp(),
            &ended,
        )
        .await;
    scene.process().await;

    assert!(
        scene.live(&record).await.is_none(),
        "the cancellation applied rather than reading as older than the write",
    );
}

/// The row lock is a lock the test can hold too, which is how it is shown to
/// be the one every change takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_waits_for_whoever_holds_the_subscription_row() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_locked",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_locked", Utc::now().timestamp() - 600)
        .await;

    let reads_before = scene.mock.reads_of("sub_locked");
    let mut holder = scene.connection().await;
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holding = tokio::spawn(async move {
        holder
            .transaction::<_, diesel::result::Error, _>(async |connection| {
                subscriptions::table
                    .filter(subscriptions::stripe_subscription_id.eq("sub_locked"))
                    .select(subscriptions::id)
                    .for_update()
                    .first::<Uuid>(connection)
                    .await?;
                let _sent = locked_tx.send(());
                let _released = release_rx.await;
                Ok(())
            })
            .await
            .expect("the holder's transaction commits");
    });
    locked_rx.await.expect("the row is locked");

    let changing = tokio::spawn({
        let scene = Arc::clone(&scene);
        let record = record.clone();
        async move {
            scene
                .subscriptions
                .change_items(
                    &Context::system(),
                    scene.organization_id,
                    &record,
                    &[ItemChange::set(BACKUPS, 1)],
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        scene.mock.reads_of("sub_locked"),
        reads_before,
        "the change takes the lock before it even reads Stripe",
    );
    assert!(scene.mock.subscription_writes().is_empty());

    let _released = release_tx.send(());
    holding.await.expect("the holder joins");
    changing
        .await
        .expect("the task must join")
        .expect("the change applies once the row is free");
    assert_eq!(scene.mock.subscription_writes().len(), 1);
}
