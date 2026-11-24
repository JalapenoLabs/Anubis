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
use axum::response::{IntoResponse as _, Response};
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
    /// Stripe's idempotency cache: a key seen before answers what it answered,
    /// a refusal included, as Stripe does for a day.
    answered: BTreeMap<String, (StatusCode, Value)>,
    /// Updates still to refuse with a `500`, saved under their key.
    refusals: u32,
    next_item: u32,
    /// Subscriptions per page of the list; Stripe's own is up to a hundred.
    page_size: usize,
    /// Items embedded in a subscription before its list says `has_more`.
    embedded_items: usize,
    /// Prices by id, as (interval, `interval_count`); one not named here bills
    /// every month.
    prices: BTreeMap<String, (String, i64)>,
    /// What goes wrong with the next update, after Stripe has applied it.
    next_update: Mishap,
    /// Whether the next read of a subscription fails with a `500`.
    fail_next_read: bool,
    /// The body each customer key was first sent with, as Stripe compares them.
    customers: BTreeMap<String, String>,
}

/// What goes wrong with an update after Stripe has applied it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mishap {
    /// Nothing: the answer arrives.
    #[default]
    Nothing,
    /// The answer is lost on its way back: Stripe saved it under its key, and
    /// the client reads garbage.
    LostAnswer,
    /// The same, and the read after it fails too, so the client learns nothing
    /// about the change it sent.
    LostAnswerAndReadBack,
    /// Stripe answers `500` and saves that under the key: an outcome it
    /// documents as unknown, here one that did apply.
    ServerError,
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
    /// Gates for the next customer creations, in order of arrival.
    customer_gates: Arc<Mutex<VecDeque<Gate>>>,
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

    /// Sets how a price recurs: every `count` of `interval`.
    fn recur(&self, price: &str, interval: &str, count: i64) {
        self.ledger()
            .prices
            .insert(price.to_owned(), (interval.to_owned(), count));
    }

    /// Loses the next update's answer, and the read after it too when `read_too`.
    fn lose_next_answer(&self, read_too: bool) {
        self.ledger().next_update = if read_too {
            Mishap::LostAnswerAndReadBack
        } else {
            Mishap::LostAnswer
        };
    }

    /// Holds the next customer creation open, once Stripe has decided its answer.
    fn hold_next_customer(&self) -> Gate {
        let gate = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        unlock(&self.customer_gates).push_back(gate.clone());
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
    let mut ledger = mock.ledger();
    if ledger.fail_next_read {
        ledger.fail_next_read = false;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({ "error": { "type": "api_error", "message": "An unknown error occurred" } }),
            ),
        );
    }
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
#[expect(
    clippy::too_many_lines,
    reason = "one Stripe endpoint: replay, refusal, the items themselves, a lost answer, a held one"
)]
async fn update(
    State(mock): State<Mock>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
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
        if let Some((status, cached)) = key.as_ref().and_then(|key| ledger.answered.get(key)) {
            // Stripe marks an answer it saved for an earlier request.
            return (
                *status,
                [("idempotent-replayed", "true")],
                Json(cached.clone()),
            )
                .into_response();
        }
        if ledger.refusals > 0 {
            ledger.refusals -= 1;
            let refusal =
                json!({ "error": { "type": "api_error", "message": "An unknown error occurred" } });
            if let Some(key) = key {
                ledger
                    .answered
                    .insert(key, (StatusCode::INTERNAL_SERVER_ERROR, refusal.clone()));
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(refusal)).into_response();
        }
        let mut subscription = ledger.subscriptions[&id].clone();

        let mut changes: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
        for (name, value) in url::form_urlencoded::parse(body.as_bytes()) {
            if name == "cancel_at_period_end" {
                subscription["cancel_at_period_end"] = json!(value == "true");
                continue;
            }
            let Some(rest) = name.strip_prefix("items[") else {
                return refused(&format!("Received unknown parameter: {name}")).into_response();
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
                        )
                        .into_response();
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
        let mishap = std::mem::take(&mut ledger.next_update);
        if mishap == Mishap::ServerError {
            let failure =
                json!({ "error": { "type": "api_error", "message": "An unknown error occurred" } });
            if let Some(key) = key {
                ledger
                    .answered
                    .insert(key, (StatusCode::INTERNAL_SERVER_ERROR, failure.clone()));
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(failure)).into_response();
        }
        if let Some(key) = key {
            ledger
                .answered
                .insert(key, (StatusCode::OK, answer.clone()));
        }
        if matches!(mishap, Mishap::LostAnswer | Mishap::LostAnswerAndReadBack) {
            ledger.fail_next_read = mishap == Mishap::LostAnswerAndReadBack;
            return (StatusCode::OK, "<the connection dropped mid-answer").into_response();
        }
        answer
    };

    let gate = unlock(&mock.update_gates).pop_front();
    if let Some((arrived, release)) = gate {
        arrived.notify_one();
        release.notified().await;
    }
    (StatusCode::OK, Json(answer)).into_response()
}

/// Answers a price, monthly unless the test said otherwise.
async fn price(State(mock): State<Mock>, Path(id): Path<String>) -> Json<Value> {
    let ledger = mock.ledger();
    let (interval, count) = ledger
        .prices
        .get(&id)
        .cloned()
        .unwrap_or_else(|| ("month".to_owned(), 1));
    Json(json!({
        "id": id, "object": "price",
        "recurring": { "interval": interval, "interval_count": count },
    }))
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

/// Creates a customer, keyed the way Stripe keys it: a key reused with the
/// same body answers the first customer, and with a different body is refused.
async fn customer(
    State(mock): State<Mock>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Json<Value>) {
    log_request(&mock, "POST", "/v1/customers".to_owned(), &headers, &body);
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let answer = {
        let mut ledger = mock.ledger();
        match ledger.customers.get(&key) {
            Some(first) if *first != body => (
                StatusCode::BAD_REQUEST,
                json!({ "error": { "type": "idempotency_error",
                    "message": "Keys for idempotent requests can only be used with the same parameters they were first used with." } }),
            ),
            _same => {
                ledger.customers.insert(key, body);
                (StatusCode::OK, json!({ "id": "cus_created" }))
            }
        }
    };

    let gate = unlock(&mock.customer_gates).pop_front();
    if let Some((arrived, release)) = gate {
        arrived.notify_one();
        release.notified().await;
    }
    (answer.0, Json(answer.1))
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
        customer_gates: Arc::default(),
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
        .route("/v1/prices/{id}", get(price))
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
        failures[1].contains(&format!("{first} already has a live subscription")),
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

    // A price that bills weekly, or every three months, is refused before
    // Stripe takes a payment the projection would never record.
    scene.mock.recur("price_weekly", "week", 1);
    scene.mock.recur("price_quarterly", "month", 3);
    for odd in ["price_weekly", "price_quarterly"] {
        let refused = scene
            .subscriptions
            .checkout(
                scene.organization_id,
                NewSubscription {
                    subject: &server(),
                    line_items: &[LineItem::new(SMALL, 1), LineItem::new(odd, 1)],
                    return_path: "/servers",
                    email: None,
                },
            )
            .await
            .expect_err("a price that is not monthly or yearly is refused");
        assert!(refused.is_invalid(), "{refused}");
        assert!(refused.message().contains(odd), "{refused}");
    }
    let sessions_before_payment = scene
        .mock
        .ledger()
        .requests
        .iter()
        .filter(|received| received.path == "/v1/checkout/sessions")
        .count();
    assert_eq!(
        sessions_before_payment, 1,
        "neither odd price reached Checkout"
    );

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

/// Two changes to one subscription never interleave, and neither is lost.
///
/// The first change's answer is held at Stripe while the second is asked for.
/// Were the second free to run, its answer (carrying both add-ons) could be
/// written first and the first one's (carrying one) after it, losing an add-on
/// the customer is paying for. It is refused instead, at once, and applies
/// when asked again once the first has finished.
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

    let refused = scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(LOGS, 1)],
        )
        .await
        .expect_err("a change while another holds the subscription is refused");
    assert!(refused.is_conflict(), "{refused}");
    assert_eq!(
        scene.mock.subscription_writes().len(),
        1,
        "the second change never raced the first to Stripe",
    );
    release.notify_one();
    first
        .await
        .expect("the task must join")
        .expect("the first change applies");

    scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(LOGS, 1)],
        )
        .await
        .expect("asked again, the second change applies");

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
/// Reconciliation reads a subscription, and while its answer is held, an
/// add-on is bought at Stripe and that change's event is processed, in the
/// same second the reconciliation's read began. The reconciliation's write,
/// carrying what it read before, lands last; it held no lock, so it loses the
/// tie, and the event's newer state stands.
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

/// A cancellation processed while an older read is in flight stays a cancellation.
///
/// The same race with the subscription cancelled at Stripe: the deletion is the
/// last event Stripe sends for it, so an older read written over it would leave
/// a cancelled subscription granting access with nothing left to repair it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancellation_during_an_older_read_is_not_undone() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_cancelled",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_cancelled", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_list();
    let reconciling = tokio::spawn({
        let scene = Arc::clone(&scene);
        async move { scene.reconciler.reconcile(scene.organization_id).await }
    });
    arrived.notified().await;

    scene.mock.set_status("sub_cancelled", "canceled");
    let ended = scene.mock.snapshot("sub_cancelled");
    scene
        .deliver(
            "evt_cancelled_mid_read",
            "customer.subscription.deleted",
            Utc::now().timestamp(),
            &ended,
        )
        .await;
    scene.process().await;
    release.notify_one();
    reconciling
        .await
        .expect("the task must join")
        .expect("reconciliation runs");

    assert!(
        scene.live(&record).await.is_none(),
        "the cancellation stands: the older read did not revive the subscription",
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
/// be the one every change takes, and that a change refuses rather than waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_refuses_while_another_holds_the_subscription_row() {
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

    let refused = tokio::time::timeout(
        Duration::from_secs(2),
        scene.subscriptions.change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        ),
    )
    .await
    .expect("the change answers at once rather than waiting on the row")
    .expect_err("the row is held, so the change is refused");
    assert!(refused.is_conflict(), "{refused}");
    let answered: anubis::http::ApiError = refused.into();
    assert_eq!(answered.status(), StatusCode::CONFLICT);
    assert_eq!(
        scene.mock.reads_of("sub_locked"),
        reads_before,
        "the change takes the lock before it even reads Stripe",
    );
    assert!(scene.mock.subscription_writes().is_empty());

    let _released = release_tx.send(());
    holding.await.expect("the holder joins");
    scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("the change applies once the row is free");
    assert_eq!(scene.mock.subscription_writes().len(), 1);
}

/// Changes piling up on a subscription another change holds cannot drain the pool.
///
/// A change holds its row across calls to Stripe. Callers retrying while it is
/// slow must not each leave a task holding a pooled connection in a queue for
/// that row, or the pool runs dry for every tenant: each is answered at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_piling_up_on_a_held_subscription_are_answered_at_once() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_slow",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_slow", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_update();
    let slow = tokio::spawn({
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

    let retries: Vec<_> = (0..5)
        .map(|_retry| {
            let scene = Arc::clone(&scene);
            let record = record.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(2),
                    scene.subscriptions.change_items(
                        &Context::system(),
                        scene.organization_id,
                        &record,
                        &[ItemChange::set(LOGS, 1)],
                    ),
                )
                .await
            })
        })
        .collect();
    for retry in retries {
        let answered = retry
            .await
            .expect("the task must join")
            .expect("each retry is answered at once rather than queued");
        let refused = answered.expect_err("the subscription is held");
        assert!(refused.is_conflict(), "{refused}");
    }
    assert_eq!(
        scene.mock.subscription_writes().len(),
        1,
        "no refused retry reached Stripe",
    );

    release.notify_one();
    slow.await
        .expect("the task must join")
        .expect("the held change still applies");
}

/// A webhook arriving while a change holds the subscription waits, then applies.
///
/// The change's answer is held at Stripe; meanwhile an item is added at Stripe
/// directly and its event is processed. The event waits on the row the change
/// holds, and once the change commits it applies, because it was created after
/// the change's read began: both the change and the dashboard's item stand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_webhook_racing_a_change_waits_for_it_and_both_stand() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_contended",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_contended", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_update();
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
    arrived.notified().await;

    scene.mock.bill("sub_contended", LOGS, 1);
    let object = scene.mock.snapshot("sub_contended");
    scene
        .deliver(
            "evt_contended",
            "customer.subscription.updated",
            Utc::now().timestamp(),
            &object,
        )
        .await;
    let processing = tokio::spawn({
        let scene = Arc::clone(&scene);
        async move { scene.process_all().await }
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut connection = scene.connection().await;
    let processed: Option<chrono::DateTime<Utc>> = stripe_billing_events::table
        .filter(stripe_billing_events::stripe_event_id.eq("evt_contended"))
        .select(stripe_billing_events::processed_at)
        .first(&mut connection)
        .await
        .expect("the event is stored");
    assert!(
        processed.is_none(),
        "the event waits for the change holding the row"
    );
    release.notify_one();

    changing
        .await
        .expect("the task must join")
        .expect("the change applies");
    for (event_id, outcome) in processing.await.expect("the task must join") {
        outcome.unwrap_or_else(|error| panic!("event {event_id} must process: {error}"));
    }

    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(
        stored(&held),
        pairs(&[(SMALL, 1), (BACKUPS, 1), (LOGS, 1)]),
        "the change and the item added at Stripe both stand",
    );
}

/// With Stripe unset or unreachable, a change says so and writes nothing.
#[tokio::test]
async fn without_a_reachable_stripe_a_change_says_so_and_writes_nothing() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let context = Context::system();
    scene.mock.hold(
        "sub_offline",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_offline", Utc::now().timestamp() - 600)
        .await;

    let unconfigured = AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _other => None,
    })
    .expect("the test config must parse");
    let offline = Subscriptions::new(scene.harness.pool.clone(), plans(), &unconfigured);
    let refused = offline
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("billing is off");
    assert!(refused.is_unconfigured(), "{refused}");
    let refused = offline
        .cancel(&context, scene.organization_id, &record, Cancellation::Now)
        .await
        .expect_err("billing is off");
    assert!(refused.is_unconfigured(), "{refused}");

    // A port nothing listens on: the request never reaches Stripe.
    let unreachable = Subscriptions::new(
        scene.harness.pool.clone(),
        plans(),
        &config("http://127.0.0.1:9"),
    );
    let failed = unreachable
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("Stripe cannot be reached");
    assert!(failed.is_transport(), "{failed}");
    let answered: anubis::http::ApiError = failed.into();
    assert_eq!(answered.status(), StatusCode::SERVICE_UNAVAILABLE);

    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1)]), "nothing was written");
    assert!(
        scene
            .audited(anubis::audit::SUBSCRIPTION_ITEMS_CHANGED)
            .await
            .is_empty(),
        "and nothing was recorded",
    );
}

/// A change Stripe refused is sent afresh next time rather than replayed.
///
/// Stripe saves a key's first answer, a `500` included, and replays it to every
/// retry with that key for a day. The refusal moves the row's revision, so the
/// next attempt is a new request; it reads Stripe back first, which is what
/// keeps a fresh key from repeating a change that did apply.
#[tokio::test]
async fn a_change_stripe_refused_is_sent_afresh_on_the_next_attempt() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let context = Context::system();
    scene.mock.hold(
        "sub_refused",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_refused", Utc::now().timestamp() - 600)
        .await;
    scene.mock.ledger().refusals = 1;

    let refused = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("Stripe refuses the first attempt");
    assert!(!refused.is_transport(), "Stripe answered: {refused}");
    let answered: anubis::http::ApiError = refused.into();
    assert_eq!(answered.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1)]), "nothing changed");
    assert!(
        scene
            .audited(anubis::audit::SUBSCRIPTION_ITEMS_CHANGED)
            .await
            .is_empty(),
        "a refused change is not recorded as made",
    );

    let held = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("the retry is a new request, and Stripe takes it");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
    let writes = scene.mock.subscription_writes();
    assert_eq!(writes.len(), 2);
    assert_ne!(
        writes[0].idempotency_key, writes[1].idempotency_key,
        "the refusal moved the key",
    );
}

/// A change whose caller goes away still finishes, and frees the row.
///
/// A request that times out drops the future awaiting the change while its
/// answer is still at Stripe. The change runs on its own task, so it reaches its
/// commit: Stripe's answer is written back, and the next change to the same
/// subscription is not left waiting on a lock nobody will release.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_whose_caller_goes_away_still_finishes_and_frees_the_row() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let record = server();
    scene.mock.hold(
        "sub_abandoned",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_abandoned", Utc::now().timestamp() - 600)
        .await;

    let (arrived, release) = scene.mock.hold_next_update();
    let abandoned = tokio::spawn({
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
    abandoned.abort();
    let _aborted = abandoned.await;
    release.notify_one();

    // Nobody awaits the abandoned change any more, so its own write is what
    // says it finished: it lands, and with it the row's lock is released.
    tokio::time::timeout(Duration::from_secs(5), async {
        while scene
            .live(&record)
            .await
            .is_none_or(|held| !stored(&held).contains(&(BACKUPS.to_owned(), 1)))
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the abandoned change finishes and writes Stripe's answer back");

    let next = scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(LOGS, 1)],
        )
        .await
        .expect("the next change finds the row free, and applies");
    assert_eq!(
        stored(&next),
        pairs(&[(SMALL, 1), (BACKUPS, 1), (LOGS, 1)]),
        "the abandoned change was written back too",
    );
}

/// The plan is every member's to read; what each record costs is not.
#[tokio::test]
async fn a_member_without_billing_authority_reads_the_plan_but_not_each_record() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_private",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene.announce("sub_private", Utc::now().timestamp()).await;

    let email = format!("member-{}@example.com", Uuid::new_v4());
    let member = register(&scene.harness.router, &email).await;
    let mut connection = scene.connection().await;
    let member_id: Uuid = anubis::schema::users::table
        .filter(anubis::schema::users::email.eq(&email))
        .select(anubis::schema::users::id)
        .first(&mut connection)
        .await
        .expect("the account exists");
    // No grant at all, so the member holds the default role and nothing else.
    diesel::insert_into(anubis::schema::organization_memberships::table)
        .values((
            anubis::schema::organization_memberships::organization_id.eq(scene.organization_id),
            anubis::schema::organization_memberships::user_id.eq(member_id),
        ))
        .execute(&mut connection)
        .await
        .expect("the membership inserts");

    let path = format!("/billing/organizations/{}", scene.organization_id);
    let (status, _headers, body) = send(&scene.billing, "GET", &path, None, Some(&member)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["plan"]["key"], "free", "body: {body}");
    assert!(
        body["subscriptions"].is_null(),
        "a member without billing authority is not shown each record's subscription: {body}",
    );

    let body = scene.billing_body().await;
    assert_eq!(
        body["subscriptions"][0]["subject_id"],
        json!(record.id().to_string()),
        "the owner is: {body}",
    );
}

/// A change whose answer is lost is read back, and recorded when Stripe applied it.
///
/// Stripe applies the add-on and the answer never arrives intact. The change is
/// still the caller's act, and the money moved, so a read-back finding it in
/// place writes it and records it in the audit log rather than failing.
#[tokio::test]
async fn a_change_whose_answer_is_lost_is_read_back_and_recorded() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_lost",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_lost", Utc::now().timestamp() - 600)
        .await;
    scene.mock.lose_next_answer(false);

    let held = scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("Stripe applied the change, so the read-back finds it");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
    let recorded = scene
        .audited(anubis::audit::SUBSCRIPTION_ITEMS_CHANGED)
        .await;
    assert_eq!(
        recorded.len(),
        1,
        "the change is in the audit log: {recorded:?}"
    );
}

/// A replayed answer to an earlier attempt is read back rather than trusted.
///
/// An add-on's answer and the read after it are both lost; then the add-on is
/// removed at Stripe directly. The retry sends the same request under the same
/// key, and Stripe replays the first attempt's saved answer, which still bills
/// the add-on. That answer describes the past: the subscription is read back,
/// the add-on is not recorded as billed, and the caller is told to try again.
#[tokio::test]
async fn a_replayed_answer_is_read_back_rather_than_trusted() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    let context = Context::system();
    scene.mock.hold(
        "sub_replayed",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_replayed", Utc::now().timestamp() - 600)
        .await;

    scene.mock.lose_next_answer(true);
    scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("neither the answer nor the read-back arrived");
    scene.mock.stop_billing("sub_replayed", BACKUPS);

    let retried = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect_err("Stripe only replayed an attempt that no longer holds");
    assert!(
        retried.is_conflict(),
        "the subscription moved; worth retrying: {retried}"
    );
    let writes = scene.mock.subscription_writes();
    assert_eq!(
        writes[0].idempotency_key, writes[1].idempotency_key,
        "the retry was the same request, which is why Stripe replayed it",
    );
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(
        stored(&held),
        pairs(&[(SMALL, 1)]),
        "the replayed answer's add-on, gone at Stripe, is not recorded",
    );

    let held = scene
        .subscriptions
        .change_items(
            &context,
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("keyed afresh, the next attempt is a new request and applies");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
}

/// Two first purchases at once, by two people, end on one customer.
///
/// Both create the organization's customer under one key, with different
/// emails, and Stripe refuses the second because its parameters differ. By the
/// time it does, the first has stored the customer, which the second then
/// uses rather than failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_first_purchases_at_once_share_one_customer() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let mut connection = scene.connection().await;
    diesel::update(organizations::table.find(scene.organization_id))
        .set(organizations::stripe_customer_id.eq(None::<String>))
        .execute(&mut connection)
        .await
        .expect("the organization starts without a customer");
    drop(connection);

    let purchase = |email: &'static str| {
        let scene = Arc::clone(&scene);
        tokio::spawn(async move {
            let record = server();
            scene
                .subscriptions
                .checkout(
                    scene.organization_id,
                    NewSubscription {
                        subject: &record,
                        line_items: &[LineItem::new(SMALL, 1)],
                        return_path: "/servers",
                        email: Some(email),
                    },
                )
                .await
        })
    };

    let (first_arrived, first_release) = scene.mock.hold_next_customer();
    let (second_arrived, second_release) = scene.mock.hold_next_customer();
    let first = purchase("first@example.com");
    first_arrived.notified().await;
    let second = purchase("second@example.com");
    second_arrived.notified().await;

    first_release.notify_one();
    first
        .await
        .expect("the task must join")
        .expect("the first purchase opens a checkout");
    second_release.notify_one();
    second
        .await
        .expect("the task must join")
        .expect("the second purchase uses the customer the first stored");

    let mut connection = scene.connection().await;
    let stored_customer: Option<String> = organizations::table
        .find(scene.organization_id)
        .select(organizations::stripe_customer_id)
        .first(&mut connection)
        .await
        .expect("the organization reads");
    assert_eq!(stored_customer.as_deref(), Some("cus_created"));
}

/// A subscription that is over is never written back to a live status.
///
/// Stripe never revives a cancelled subscription, so a live status arriving
/// after the cancellation is an older read whatever its stamp says, here an
/// event whose read-back saw Stripe before the cancellation reached it.
#[tokio::test]
async fn a_subscription_that_is_over_is_never_revived() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_over",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    let now = Utc::now().timestamp();
    scene.announce("sub_over", now - 600).await;

    let mut ended = scene.mock.snapshot("sub_over");
    ended["status"] = json!("canceled");
    scene
        .deliver(
            "evt_over",
            "customer.subscription.deleted",
            now - 60,
            &ended,
        )
        .await;
    scene.process().await;
    assert!(scene.live(&record).await.is_none());

    scene.announce("sub_over", now).await;
    assert!(
        scene.live(&record).await.is_none(),
        "a later read still showing it active does not bring it back",
    );
}

/// A subscription on a price billing every several months is recorded, and so is its end.
///
/// The framework's own checkout refuses such a price, but one can still arrive
/// from the Stripe dashboard or the customer portal. The projection records
/// whatever Stripe reports, because a subscription it cannot record is one whose
/// cancellation it cannot record either, and a deletion is the last event
/// Stripe sends.
#[tokio::test]
async fn a_subscription_billing_every_several_months_is_recorded_and_so_is_its_end() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_quarterly",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .mock
        .ledger()
        .subscriptions
        .get_mut("sub_quarterly")
        .expect("held")["items"]["data"][0]["price"]["recurring"]["interval_count"] = json!(3);
    let now = Utc::now().timestamp();

    scene.announce("sub_quarterly", now - 600).await;
    assert!(
        scene.live(&record).await.is_some(),
        "the subscription Stripe reports is recorded",
    );

    let mut ended = scene.mock.snapshot("sub_quarterly");
    ended["status"] = json!("canceled");
    scene
        .deliver(
            "evt_quarterly_deleted",
            "customer.subscription.deleted",
            now,
            &ended,
        )
        .await;
    scene.process().await;
    assert!(
        scene.live(&record).await.is_none(),
        "and its cancellation is recorded too",
    );
}

/// A first purchase refused before the other stored its customer is told to retry.
///
/// The other ordering of the race the shared customer key resolves: the second
/// purchase's refusal comes back while the first is still creating the
/// customer, so nothing is stored to fall back on yet. That is a conflict a
/// retry resolves, not a server error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_purchase_refused_before_the_other_stored_its_customer_is_told_to_retry() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    let mut connection = scene.connection().await;
    diesel::update(organizations::table.find(scene.organization_id))
        .set(organizations::stripe_customer_id.eq(None::<String>))
        .execute(&mut connection)
        .await
        .expect("the organization starts without a customer");
    drop(connection);

    let purchase = |email: &'static str| {
        let scene = Arc::clone(&scene);
        async move {
            let record = server();
            scene
                .subscriptions
                .checkout(
                    scene.organization_id,
                    NewSubscription {
                        subject: &record,
                        line_items: &[LineItem::new(SMALL, 1)],
                        return_path: "/servers",
                        email: Some(email),
                    },
                )
                .await
        }
    };

    let (arrived, release) = scene.mock.hold_next_customer();
    let first = tokio::spawn(purchase("first@example.com"));
    arrived.notified().await;

    let refused = purchase("second@example.com")
        .await
        .expect_err("the first purchase has not stored its customer yet");
    assert!(refused.is_conflict(), "a retry resolves it: {refused}");
    let answered: anubis::http::ApiError = refused.into();
    assert_eq!(answered.status(), StatusCode::CONFLICT);

    release.notify_one();
    first
        .await
        .expect("the task must join")
        .expect("the first purchase opens a checkout");
    purchase("second@example.com")
        .await
        .expect("asked again, the second purchase uses the stored customer");
}

/// A change onto a price billing weekly is refused before Stripe hears of it.
///
/// The same rule checkout applies: a subscription moved onto such a price
/// would be charged and then could not be recorded, nor could its end.
#[tokio::test]
async fn a_change_onto_a_price_billing_weekly_is_refused_before_stripe_hears_of_it() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_swap",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_swap", Utc::now().timestamp() - 600)
        .await;
    scene.mock.recur("price_weekly", "week", 1);

    let refused = scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[
                ItemChange::remove(SMALL),
                ItemChange::set("price_weekly", 1),
            ],
        )
        .await
        .expect_err("a price billing weekly is refused");
    assert!(refused.is_invalid(), "{refused}");
    assert!(refused.message().contains("price_weekly"), "{refused}");
    assert!(
        scene.mock.subscription_writes().is_empty(),
        "Stripe heard nothing"
    );
    let held = scene.live(&record).await.expect("still paid for");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1)]));
}

/// A change answered with a server error is read back, and recorded when it applied.
///
/// Stripe documents a `500` as an outcome it does not know: the change may
/// have been made. Like a lost answer, it is read back, and a change Stripe
/// shows in place is written and recorded rather than reported as failed.
#[tokio::test]
async fn a_change_answered_with_a_server_error_is_read_back_and_recorded() {
    let Some(database) = TestDatabase::create("billing_subscriptions_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let record = server();
    scene.mock.hold(
        "sub_unknown",
        &scene.customer,
        "active",
        &scene.record_metadata(&record),
        &[(SMALL, 1)],
    );
    scene
        .announce("sub_unknown", Utc::now().timestamp() - 600)
        .await;
    scene.mock.ledger().next_update = Mishap::ServerError;

    let held = scene
        .subscriptions
        .change_items(
            &Context::system(),
            scene.organization_id,
            &record,
            &[ItemChange::set(BACKUPS, 1)],
        )
        .await
        .expect("Stripe applied the change, so the read-back finds it");
    assert_eq!(stored(&held), pairs(&[(SMALL, 1), (BACKUPS, 1)]));
    let recorded = scene
        .audited(anubis::audit::SUBSCRIPTION_ITEMS_CHANGED)
        .await;
    assert_eq!(
        recorded.len(),
        1,
        "the change is in the audit log: {recorded:?}"
    );
}
