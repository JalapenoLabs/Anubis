//! The prepaid credit wallet, end to end.
//!
//! Top-ups and refunds arrive as signed events through the real receiver and
//! are processed by the real job; spends go through [`Wallets::spend`];
//! auto-recharge runs the real [`RechargeWallet`] job; the billing endpoints
//! answer over HTTP. Stripe is a stateful mock that holds payments and
//! refunds, answers off-session charges, keeps an idempotency cache the way
//! Stripe does, and can decline, lose an answer, or save a server error under
//! a key, so every behavior asserted here is one the code under test had to
//! earn against Stripe's own shapes.
//!
//! The races run for real: spends and credits spawned on a multi-threaded
//! runtime contend for one wallet's row lock, and the invariant asserted is
//! the one that matters, that the cached balance always equals the ledger's
//! sum and that one dip below the threshold charges the card once.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use anubis::billing::{
    PlanSet, ProcessStripeEvent, RechargeWallet, Reconciler, Spend, Subject, Wallets,
};
use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::schema::{
    audit_events, jobs, notifications, organization_memberships, organizations,
    stripe_billing_events, wallet_entries, wallets,
};
use anubis::webhooks::signature;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
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
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [default, billing]
    models:
      Team: [manage]
";

/// A free plan of one seat, and a paid plan of a hundred that no subscription
/// sells: paying for anything is the only way onto it.
const BILLING_YML: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 1
  - key: member
    name: Member
    limits:
      seats: 100

paid_plan: member

wallet:
  name: Account credit
  currency: usd
  minimum_top_up_micros: 10000000
  low_balance_micros: 2000000
";

const WEBHOOK_SECRET: &str = "whsec_billing_wallet_flow";
const RECEIVER: &str = "/webhooks/stripe-billing";

/// One dollar, in the wallet's unit.
const DOLLAR: i64 = 1_000_000;

fn unlock<Shape>(guarded: &Mutex<Shape>) -> MutexGuard<'_, Shape> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// What goes wrong with the next off-session charge.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mishap {
    /// Nothing: the card is charged and the answer arrives.
    #[default]
    Nothing,
    /// The card is declined: `402`, a `card_error`, and the payment left
    /// waiting for another payment method.
    Decline,
    /// The card is charged and the answer is lost on its way back.
    LostAnswer,
    /// Stripe answers `500`, charges nothing, and saves that under the key.
    ServerError,
}

/// Everything the mock Stripe holds.
#[derive(Default)]
struct Ledger {
    /// Payments by id.
    payments: BTreeMap<String, Value>,
    /// Refunds by id.
    refunds: BTreeMap<String, Value>,
    /// Stripe's idempotency cache: a key seen before answers what it answered.
    answered: BTreeMap<String, (StatusCode, Value)>,
    next_update: Mishap,
    /// Every Checkout session form, as it arrived.
    sessions: Vec<String>,
    /// How many off-session charges Stripe actually carried out.
    charges: u32,
    next_id: u32,
}

#[derive(Clone, Default)]
struct Mock {
    ledger: Arc<Mutex<Ledger>>,
}

impl Mock {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        unlock(&self.ledger)
    }

    /// Puts a payment at Stripe.
    fn pay(&self, id: &str, customer: &str, metadata: &Value, amount_minor: i64, extra: &Value) {
        let mut payment = json!({
            "id": id, "object": "payment_intent", "status": "succeeded",
            "amount": amount_minor, "amount_received": amount_minor, "currency": "usd",
            "customer": customer, "created": Utc::now().timestamp(),
            "metadata": metadata, "latest_charge": format!("ch_{id}"),
        });
        if let (Some(target), Some(fields)) = (payment.as_object_mut(), extra.as_object()) {
            target.extend(fields.clone());
        }
        self.ledger().payments.insert(id.to_owned(), payment);
    }

    /// Puts a refund of a payment at Stripe.
    fn refund(&self, id: &str, payment: &str, amount_minor: i64, status: &str) {
        self.ledger().refunds.insert(
            id.to_owned(),
            json!({
                "id": id, "object": "refund", "amount": amount_minor, "currency": "usd",
                "payment_intent": payment, "charge": format!("ch_{payment}"), "status": status,
            }),
        );
    }

    fn payment(&self, id: &str) -> Value {
        self.ledger().payments[id].clone()
    }
}

async fn retrieve_payment(
    State(mock): State<Mock>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    match mock.ledger().payments.get(&id) {
        Some(payment) => (StatusCode::OK, Json(payment.clone())),
        None => (
            StatusCode::NOT_FOUND,
            Json(
                json!({ "error": { "message": "No such payment_intent", "code": "resource_missing" } }),
            ),
        ),
    }
}

/// Lists a customer's payments newest first, as Stripe does.
async fn list_payments(
    State(mock): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let ledger = mock.ledger();
    let since: i64 = query
        .get("created[gte]")
        .map_or(0, |value| value.parse().expect("a timestamp"));
    let mut held: Vec<Value> = ledger
        .payments
        .values()
        .filter(|payment| {
            payment["customer"].as_str() == query.get("customer").map(String::as_str)
                && payment["created"].as_i64().unwrap_or_default() >= since
        })
        .cloned()
        .collect();
    held.reverse();
    Json(json!({ "object": "list", "data": held, "has_more": false }))
}

/// Charges a saved card off-session, keyed the way Stripe keys it.
async fn create_payment(State(mock): State<Mock>, headers: HeaderMap, body: String) -> Response {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .expect("a recharge is always keyed")
        .to_owned();
    let mut ledger = mock.ledger();
    if let Some((status, cached)) = ledger.answered.get(&key) {
        return (
            *status,
            [("idempotent-replayed", "true")],
            Json(cached.clone()),
        )
            .into_response();
    }

    let form: BTreeMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        form["off_session"], "true",
        "a recharge is charged off-session"
    );
    assert_eq!(form["confirm"], "true");
    let mishap = std::mem::take(&mut ledger.next_update);
    if mishap == Mishap::ServerError {
        let failure =
            json!({ "error": { "type": "api_error", "message": "An unknown error occurred" } });
        ledger
            .answered
            .insert(key, (StatusCode::INTERNAL_SERVER_ERROR, failure.clone()));
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(failure)).into_response();
    }

    ledger.next_id += 1;
    let id = format!("pi_recharge_{}", ledger.next_id);
    let metadata: BTreeMap<String, String> = form
        .iter()
        .filter_map(|(name, value)| {
            name.strip_prefix("metadata[")
                .and_then(|rest| rest.strip_suffix(']'))
                .map(|name| (name.to_owned(), value.clone()))
        })
        .collect();
    let amount: i64 = form["amount"].parse().expect("an amount");
    let declined = mishap == Mishap::Decline;
    let payment = json!({
        "id": id, "object": "payment_intent",
        "status": if declined { "requires_payment_method" } else { "succeeded" },
        "amount": amount, "amount_received": if declined { 0 } else { amount },
        "currency": form["currency"], "customer": form["customer"],
        "payment_method": form["payment_method"], "created": Utc::now().timestamp(),
        "metadata": metadata,
    });
    ledger.payments.insert(id, payment.clone());
    if declined {
        let refusal = json!({ "error": {
            "type": "card_error", "code": "card_declined", "decline_code": "insufficient_funds",
            "message": "Your card has insufficient funds.", "payment_intent": payment,
        } });
        ledger
            .answered
            .insert(key, (StatusCode::PAYMENT_REQUIRED, refusal.clone()));
        return (StatusCode::PAYMENT_REQUIRED, Json(refusal)).into_response();
    }
    ledger.charges += 1;
    ledger
        .answered
        .insert(key, (StatusCode::OK, payment.clone()));
    if mishap == Mishap::LostAnswer {
        return (StatusCode::OK, "<the connection dropped mid-answer").into_response();
    }
    (StatusCode::OK, Json(payment)).into_response()
}

async fn list_refunds(
    State(mock): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let ledger = mock.ledger();
    let mut held: Vec<Value> = ledger
        .refunds
        .values()
        .filter(|refund| {
            refund["payment_intent"].as_str() == query.get("payment_intent").map(String::as_str)
        })
        .cloned()
        .collect();
    held.reverse();
    Json(json!({ "object": "list", "data": held, "has_more": false }))
}

async fn create_session(State(mock): State<Mock>, body: String) -> Json<Value> {
    mock.ledger().sessions.push(body);
    Json(json!({ "id": "cs_test_top_up", "url": "https://checkout.stripe.test/cs_test_top_up" }))
}

async fn no_subscriptions() -> Json<Value> {
    Json(json!({ "object": "list", "data": [], "has_more": false }))
}

async fn start_mock_stripe() -> (String, Mock) {
    let mock = Mock::default();
    let router = Router::new()
        .route("/v1/checkout/sessions", post(create_session))
        .route(
            "/v1/payment_intents",
            get(list_payments).post(create_payment),
        )
        .route("/v1/payment_intents/{id}", get(retrieve_payment))
        .route("/v1/refunds", get(list_refunds))
        .route("/v1/subscriptions", get(no_subscriptions))
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

fn config(api_base: Option<&str>) -> AppConfig {
    let api_base = api_base.map(str::to_owned);
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("https://app.example.com".to_owned()),
        "STRIPE_SECRET_KEY" => api_base
            .as_ref()
            .map(|_base| "sk_test_wallet_flow".to_owned()),
        "STRIPE_API_BASE" => api_base.clone(),
        "STRIPE_WEBHOOK_SECRET" => api_base.as_ref().map(|_base| WEBHOOK_SECRET.to_owned()),
        _other => None,
    })
    .expect("the test config must parse")
}

fn plans() -> PlanSet {
    PlanSet::from_yaml(BILLING_YML).expect("plans must parse")
}

/// One narrative's world: the billing surface, the jobs, the wallet, the mock,
/// and an organization with a Stripe customer.
struct Scene {
    harness: Harness,
    billing: Router,
    reconciler: Reconciler,
    wallets: Wallets,
    mock: Mock,
    owner: String,
    organization_id: Uuid,
    customer: String,
}

impl Scene {
    async fn new(database: &TestDatabase) -> Self {
        let (stripe_base, mock) = start_mock_stripe().await;
        Self::with_stripe(database, Some(&stripe_base), mock).await
    }

    async fn with_stripe(database: &TestDatabase, stripe_base: Option<&str>, mock: Mock) -> Self {
        let harness = Harness::boot_with_plans(database, plans()).await;
        let config = config(stripe_base);
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
        let wallets = Wallets::new(plans(), &config);

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
            wallets,
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

    fn wallet_metadata(&self, purpose: &str) -> Value {
        json!({ "organization_id": self.organization_id.to_string(), "wallet": purpose })
    }

    /// Delivers one signed event, then runs every stored event that waits.
    async fn deliver(&self, event_id: &str, event_type: &str, object: &Value) {
        let event = json!({
            "id": event_id, "object": "event", "type": event_type,
            "created": Utc::now().timestamp(), "data": { "object": object },
        });
        let (status, body) = deliver(&self.billing, &event).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        self.process().await;
    }

    async fn process(&self) {
        let mut connection = self.connection().await;
        let waiting: Vec<Uuid> = stripe_billing_events::table
            .filter(stripe_billing_events::processed_at.is_null())
            .order(stripe_billing_events::received_at.asc())
            .select(stripe_billing_events::id)
            .load(&mut connection)
            .await
            .expect("the backlog must be readable");
        for event_id in waiting {
            self.reconciler
                .process(ProcessStripeEvent { event_id })
                .await
                .unwrap_or_else(|error| panic!("event {event_id} must process: {error}"));
        }
    }

    /// A top-up paid at Stripe and announced by its payment's own event.
    async fn top_up(&self, payment_id: &str, amount_minor: i64, save_card: bool) {
        let extra = if save_card {
            json!({ "setup_future_usage": "off_session", "payment_method": "pm_card_visa" })
        } else {
            json!({})
        };
        self.mock.pay(
            payment_id,
            &self.customer,
            &self.wallet_metadata("top_up"),
            amount_minor,
            &extra,
        );
        let object = self.mock.payment(payment_id);
        self.deliver(
            &format!("evt_{payment_id}"),
            "payment_intent.succeeded",
            &object,
        )
        .await;
    }

    async fn spend(
        &self,
        amount_micros: i64,
        key: &str,
    ) -> Result<anubis::billing::Spent, anubis::billing::wallet::Error> {
        let mut connection = self.connection().await;
        self.wallets
            .spend(
                &mut connection,
                self.organization_id,
                Spend {
                    amount_micros,
                    key,
                    description: Some("one hour of a server"),
                    subject: None,
                },
            )
            .await
    }

    async fn balance(&self) -> i64 {
        let mut connection = self.connection().await;
        self.wallets
            .balance(&mut connection, self.organization_id)
            .await
            .expect("the balance must read")
    }

    /// The ledger's sum, which the cached balance must always equal.
    async fn ledger_sum(&self) -> i64 {
        let mut connection = self.connection().await;
        let amounts: Vec<i64> = wallet_entries::table
            .filter(wallet_entries::organization_id.eq(self.organization_id))
            .select(wallet_entries::amount_micros)
            .load(&mut connection)
            .await
            .expect("the ledger must read");
        amounts.into_iter().sum()
    }

    /// Every entry's kind and amount, oldest first.
    async fn entries(&self) -> Vec<(String, i64)> {
        let mut connection = self.connection().await;
        wallet_entries::table
            .filter(wallet_entries::organization_id.eq(self.organization_id))
            .order(wallet_entries::position.asc())
            .select((wallet_entries::kind, wallet_entries::amount_micros))
            .load(&mut connection)
            .await
            .expect("the ledger must read")
    }

    /// How many recharge jobs wait on the queue.
    async fn recharges_queued(&self) -> i64 {
        let mut connection = self.connection().await;
        jobs::table
            .filter(jobs::kind.eq("anubis.billing.recharge_wallet"))
            .count()
            .get_result(&mut connection)
            .await
            .expect("the queue must read")
    }

    async fn recharge(&self) -> Result<(), String> {
        self.reconciler
            .recharge_wallet(RechargeWallet {
                organization_id: self.organization_id,
            })
            .await
            .map_err(|error| error.to_string())
    }

    async fn audited(&self, action: &str) -> i64 {
        let mut connection = self.connection().await;
        audit_events::table
            .filter(audit_events::organization_id.eq(self.organization_id))
            .filter(audit_events::action.eq(action))
            .count()
            .get_result(&mut connection)
            .await
            .expect("the audit log must read")
    }

    async fn notified(&self, kind: &str) -> i64 {
        let mut connection = self.connection().await;
        notifications::table
            .filter(notifications::kind.eq(kind))
            .filter(
                notifications::href.eq(format!("/organizations/{}/billing", self.organization_id)),
            )
            .count()
            .get_result(&mut connection)
            .await
            .expect("the inbox must read")
    }

    async fn billing_body(&self, cookie: &str) -> (StatusCode, Value) {
        let (status, _headers, body) = send(
            &self.billing,
            "GET",
            &format!("/billing/organizations/{}", self.organization_id),
            None,
            Some(cookie),
        )
        .await;
        (status, body)
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        cookie: &str,
    ) -> (StatusCode, Value) {
        let (status, _headers, body) = send(
            &self.billing,
            method,
            &format!("/billing/organizations/{}{path}", self.organization_id),
            body.as_ref(),
            Some(cookie),
        )
        .await;
        (status, body)
    }

    /// Turns auto-recharge on as the owner: below $5, buy $10.
    async fn enable_auto_recharge(&self) {
        let (status, body) = self
            .request(
                "PUT",
                "/wallet/auto-recharge",
                Some(json!({ "threshold_micros": 5 * DOLLAR, "amount_micros": 10 * DOLLAR })),
                &self.owner,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    /// A second account in the organization, holding only the default role.
    async fn plain_member(&self) -> String {
        let cookie = register(
            &self.harness.router,
            &format!("member-{}@example.com", Uuid::new_v4()),
        )
        .await;
        let (status, _headers, me) =
            send(&self.harness.router, "GET", "/auth/me", None, Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK);
        let user_id: Uuid = me["user"]["id"]
            .as_str()
            .expect("an id")
            .parse()
            .expect("a uuid");
        let mut connection = self.connection().await;
        diesel::insert_into(organization_memberships::table)
            .values((
                organization_memberships::organization_id.eq(self.organization_id),
                organization_memberships::user_id.eq(user_id),
                organization_memberships::roles.eq(vec!["default".to_owned()]),
            ))
            .execute(&mut connection)
            .await
            .expect("the membership must insert");
        cookie
    }
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one narrative: the checkout opened, then every event that could credit it twice"
)]
async fn a_top_up_is_opened_at_checkout_and_credits_once_however_its_events_arrive() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;

    // Below the minimum, and a fraction of a cent, are refused before Stripe hears of them.
    for amount in [5 * DOLLAR, 10 * DOLLAR + 1] {
        let (status, body) = scene
            .request(
                "POST",
                "/wallet/top-up",
                Some(json!({ "amount_micros": amount })),
                &scene.owner,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{amount}: {body}");
    }
    assert!(scene.mock.ledger().sessions.is_empty());

    let (status, body) = scene
        .request(
            "POST",
            "/wallet/top-up",
            Some(json!({ "amount_micros": 25 * DOLLAR, "save_card": true })),
            &scene.owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["url"], "https://checkout.stripe.test/cs_test_top_up");
    let session: BTreeMap<String, String> =
        url::form_urlencoded::parse(scene.mock.ledger().sessions[0].as_bytes())
            .into_owned()
            .collect();
    assert_eq!(session["mode"], "payment");
    assert_eq!(session["customer"], scene.customer);
    assert_eq!(
        session["line_items[0][price_data][unit_amount]"], "2500",
        "$25.00 in cents"
    );
    assert_eq!(session["line_items[0][price_data][currency]"], "usd");
    assert_eq!(
        session["line_items[0][price_data][product_data][name]"],
        "Account credit"
    );
    assert_eq!(session["payment_intent_data[metadata][wallet]"], "top_up");
    assert_eq!(
        session["payment_intent_data[metadata][organization_id]"],
        scene.organization_id.to_string(),
    );
    assert_eq!(
        session["payment_intent_data[setup_future_usage]"],
        "off_session"
    );
    assert!(session["success_url"].ends_with("?top_up=success"));

    // Stripe charged $25. The session event says otherwise, and is not believed.
    scene.mock.pay(
        "pi_top_up",
        &scene.customer,
        &scene.wallet_metadata("top_up"),
        2_500,
        &json!({ "setup_future_usage": "off_session", "payment_method": "pm_card_visa" }),
    );
    let session_object = json!({
        "id": "cs_test_top_up", "object": "checkout.session", "mode": "payment",
        "payment_intent": "pi_top_up", "amount_total": 999_999,
        "metadata": scene.wallet_metadata("top_up"),
    });
    scene
        .deliver("evt_session", "checkout.session.completed", &session_object)
        .await;
    // Redelivered, then announced again by the payment's own event, and the
    // async-payment event too: three more chances to credit twice.
    scene
        .deliver("evt_session", "checkout.session.completed", &session_object)
        .await;
    let payment = scene.mock.payment("pi_top_up");
    scene
        .deliver("evt_payment", "payment_intent.succeeded", &payment)
        .await;
    scene
        .deliver(
            "evt_async",
            "checkout.session.async_payment_succeeded",
            &session_object,
        )
        .await;

    assert_eq!(scene.entries().await, [("top_up".to_owned(), 25 * DOLLAR)]);
    assert_eq!(scene.balance().await, 25 * DOLLAR);
    assert_eq!(scene.audited("wallet.topped_up").await, 1);

    let (status, body) = scene.billing_body(&scene.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["wallet"]["balance_micros"], 25 * DOLLAR);
    assert_eq!(body["wallet"]["currency"], "usd");
    assert_eq!(
        body["wallet"]["card_saved"], true,
        "the top-up saved its card"
    );
    assert_eq!(
        body["plan"]["key"], "member",
        "a positive balance is paying for something"
    );
    assert_eq!(body["plan_source"], "paid_status");

    let (status, ledger) = scene
        .request("GET", "/wallet/entries", None, &scene.owner)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {ledger}");
    assert_eq!(ledger["pagination"]["total_items"], 1);
    assert_eq!(ledger["entries"][0]["kind"], "top_up");
    assert_eq!(ledger["entries"][0]["balance_after_micros"], 25 * DOLLAR);
}

#[tokio::test]
async fn spends_drain_to_zero_replay_by_key_and_move_the_plan() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    let limits = anubis::billing::Limits::new(plans());
    let plan_now = || async {
        let mut connection = scene.connection().await;
        limits
            .plan_for(&mut connection, scene.organization_id)
            .await
            .expect("the plan must resolve")
            .key()
            .to_owned()
    };

    // No wallet at all: nothing to spend, and the free plan.
    let refused = scene
        .spend(DOLLAR, "hour-0")
        .await
        .expect_err("an empty wallet");
    assert!(refused.is_insufficient(), "{refused}");
    assert_eq!(refused.balance_micros(), Some(0));
    assert_eq!(plan_now().await, "free");

    scene.top_up("pi_seed", 1_000, false).await;
    assert_eq!(
        plan_now().await,
        "member",
        "$10 of credit puts it on the paid plan"
    );

    // A per-hour charge far below a cent is recorded exactly.
    let spent = scene.spend(87_500, "hour-1").await.expect("covered");
    assert_eq!(spent.charged_micros(), 87_500);
    assert_eq!(spent.uncovered_micros(), 0);
    assert_eq!(spent.balance_micros(), 10 * DOLLAR - 87_500);

    // The meter retries after a lost answer: one charge.
    let replayed = scene.spend(87_500, "hour-1").await.expect("a replay");
    assert!(replayed.is_replay());
    assert_eq!(replayed.entry().id, spent.entry().id);
    // The same key for a different charge is a bug in the meter, said so.
    let conflict = scene
        .spend(90_000, "hour-1")
        .await
        .expect_err("a reused key");
    assert!(conflict.is_conflict(), "{conflict}");

    // More than is left: the wallet drains to exactly zero and says how much it missed.
    let drained = scene.spend(11 * DOLLAR, "hour-2").await.expect("drains");
    assert_eq!(drained.charged_micros(), 10 * DOLLAR - 87_500);
    assert_eq!(drained.uncovered_micros(), DOLLAR + 87_500);
    assert_eq!(drained.balance_micros(), 0);
    assert_eq!(
        plan_now().await,
        "free",
        "a balance of zero pays for nothing"
    );

    let refused = scene.spend(1, "hour-3").await.expect_err("nothing left");
    assert!(refused.is_insufficient());
    // A refusal writes nothing, so the same key spends once credit arrives.
    scene.top_up("pi_again", 1_000, false).await;
    scene.spend(1, "hour-3").await.expect("credit arrived");

    assert_eq!(scene.balance().await, scene.ledger_sum().await);
    // Crossing $2 told the owner once, on the way down.
    assert_eq!(scene.notified("wallet.low_balance").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_spends_and_credits_keep_the_balance_equal_to_the_ledger() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    scene.top_up("pi_start", 1_000, false).await;

    // Forty spends of a quarter and five $10 credits, all at once: $10 + $50 -
    // forty quarters = $50, whatever order the row lock admits them in.
    let mut racers = Vec::new();
    for index in 0..40 {
        let scene = Arc::clone(&scene);
        racers.push(tokio::spawn(async move {
            scene
                .spend(DOLLAR / 4, &format!("race-{index}"))
                .await
                .map(|_spent| ())
                .map_err(|error| error.to_string())
        }));
    }
    for index in 0..5 {
        let scene = Arc::clone(&scene);
        racers.push(tokio::spawn(async move {
            // Each credit arrives by reconciliation, which converges every
            // payment the customer holds: five of them racing each other over
            // the same payments as well as the spends.
            let id = format!("pi_race_{index}");
            scene.mock.pay(
                &id,
                &scene.customer,
                &scene.wallet_metadata("top_up"),
                1_000,
                &json!({}),
            );
            scene
                .reconciler
                .reconcile(scene.organization_id)
                .await
                .map(|_live| ())
                .map_err(|error| error.to_string())
        }));
    }
    for racer in racers {
        racer
            .await
            .expect("the task must join")
            .expect("every racer succeeds");
    }

    assert_eq!(scene.balance().await, 50 * DOLLAR);
    assert_eq!(scene.ledger_sum().await, 50 * DOLLAR);
    let entries = scene.entries().await;
    assert_eq!(
        entries.len(),
        46,
        "one top-up, forty spends, five credits, each once"
    );

    // Each entry's balance chains from the one before it.
    let mut connection = scene.connection().await;
    let chain: Vec<(i64, i64)> = wallet_entries::table
        .filter(wallet_entries::organization_id.eq(scene.organization_id))
        .order(wallet_entries::position.asc())
        .select((
            wallet_entries::amount_micros,
            wallet_entries::balance_after_micros,
        ))
        .load(&mut connection)
        .await
        .expect("the ledger must read");
    let mut running = 0;
    for (amount, after) in chain {
        running += amount;
        assert_eq!(after, running, "the ledger chains");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spends_racing_below_the_threshold_charge_the_card_once() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Arc::new(Scene::new(&database).await);
    // $6 on the wallet and a saved card; auto-recharge below $5, buying $10.
    scene.top_up("pi_card", 1_000, true).await;
    scene.spend(4 * DOLLAR, "setup").await.expect("covered");
    scene.enable_auto_recharge().await;
    assert_eq!(
        scene.recharges_queued().await,
        0,
        "$6 is above the threshold"
    );

    let mut racers = Vec::new();
    for index in 0..10 {
        let scene = Arc::clone(&scene);
        racers.push(tokio::spawn(async move {
            scene
                .spend(DOLLAR / 2, &format!("dip-{index}"))
                .await
                .map(|_spent| ())
        }));
    }
    for racer in racers {
        racer
            .await
            .expect("the task must join")
            .expect("every spend is covered");
    }
    assert_eq!(
        scene.recharges_queued().await,
        1,
        "ten spends below the threshold, one recharge"
    );
    assert_eq!(scene.balance().await, DOLLAR);

    // Even two workers running it at once charge once.
    let (first, second) = tokio::join!(scene.recharge(), scene.recharge());
    first.expect("the recharge runs");
    second.expect("the second run finds the first's charge, or nothing armed");
    scene
        .recharge()
        .await
        .expect("a third run finds nothing armed");
    assert_eq!(scene.mock.ledger().charges, 1, "one dip, one charge");
    assert_eq!(scene.balance().await, 11 * DOLLAR);
    assert_eq!(scene.audited("wallet.recharged").await, 1);
    // A low-balance notice is not sent while auto-recharge answers it.
    assert_eq!(scene.notified("wallet.low_balance").await, 0);

    let (_status, body) = scene.billing_body(&scene.owner).await;
    assert_eq!(
        body["wallet"]["recharging"], false,
        "the attempt is settled"
    );
    assert_eq!(scene.balance().await, scene.ledger_sum().await);
}

#[tokio::test]
async fn a_declined_card_turns_auto_recharge_off_once_and_never_loops() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    scene.top_up("pi_card", 1_000, true).await;
    scene.enable_auto_recharge().await;

    scene.mock.ledger().next_update = Mishap::Decline;
    scene.spend(6 * DOLLAR, "dip").await.expect("covered");
    scene
        .recharge()
        .await
        .expect("a decline is not a job failure");

    let (_status, body) = scene.billing_body(&scene.owner).await;
    assert_eq!(body["wallet"]["auto_recharge"], Value::Null, "turned off");
    assert!(body["wallet"]["recharge_failed_at"].is_string());
    assert_eq!(scene.notified("wallet.recharge_failed").await, 1);
    assert_eq!(scene.audited("wallet.auto_recharge_failed").await, 1);
    assert_eq!(scene.mock.ledger().charges, 0);

    // More spends below the threshold arm nothing: the card is not tried again.
    let queued = scene.recharges_queued().await;
    scene.spend(DOLLAR, "after").await.expect("covered");
    scene.spend(DOLLAR, "after-2").await.expect("covered");
    assert_eq!(scene.recharges_queued().await, queued);
    scene.recharge().await.expect("nothing armed");
    assert_eq!(
        scene.notified("wallet.recharge_failed").await,
        1,
        "told once"
    );
}

#[tokio::test]
async fn a_recharge_whose_answer_is_lost_or_saved_as_an_error_is_read_back() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    scene.top_up("pi_card", 1_000, true).await;
    scene.enable_auto_recharge().await;

    // The card is charged and the answer lost: found by its attempt, credited once.
    scene.mock.ledger().next_update = Mishap::LostAnswer;
    scene.spend(6 * DOLLAR, "dip-1").await.expect("covered");
    scene
        .recharge()
        .await
        .expect("the lost charge is found again");
    assert_eq!(scene.mock.ledger().charges, 1);
    assert_eq!(scene.balance().await, 14 * DOLLAR);

    // Stripe saves a server error under the key and charged nothing: the job
    // fails, and its retry goes out under a fresh key instead of fetching the
    // saved error back.
    scene.spend(10 * DOLLAR, "dip-2").await.expect("covered");
    scene.mock.ledger().next_update = Mishap::ServerError;
    let failed = scene
        .recharge()
        .await
        .expect_err("an unknown outcome retries");
    assert!(failed.contains("500"), "{failed}");
    scene.recharge().await.expect("the retry charges");
    assert_eq!(scene.mock.ledger().charges, 2);
    assert_eq!(scene.balance().await, 14 * DOLLAR);
    assert_eq!(scene.balance().await, scene.ledger_sum().await);
}

#[tokio::test]
async fn refunds_debit_once_in_any_order_and_a_failed_refund_comes_back() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;

    // The refund's event is processed before the top-up's own: the payment is
    // converged first, so the credit is there to refund from.
    scene.mock.pay(
        "pi_refunded",
        &scene.customer,
        &scene.wallet_metadata("top_up"),
        2_000,
        &json!({}),
    );
    scene.mock.refund("re_1", "pi_refunded", 500, "pending");
    let refund = json!({ "id": "re_1", "object": "refund", "payment_intent": "pi_refunded", "status": "succeeded" });
    scene
        .deliver("evt_refund_created", "refund.created", &refund)
        .await;
    let charge =
        json!({ "id": "ch_pi_refunded", "object": "charge", "payment_intent": "pi_refunded" });
    scene
        .deliver("evt_charge_refunded", "charge.refunded", &charge)
        .await;
    scene
        .deliver("evt_refund_created", "refund.created", &refund)
        .await;
    let payment = scene.mock.payment("pi_refunded");
    scene
        .deliver("evt_paid_late", "payment_intent.succeeded", &payment)
        .await;

    assert_eq!(
        scene.entries().await,
        [
            ("top_up".to_owned(), 20 * DOLLAR),
            ("refund".to_owned(), -5 * DOLLAR)
        ],
    );

    // The refund fails at the bank: its credit comes back, once.
    scene.mock.refund("re_1", "pi_refunded", 500, "failed");
    scene
        .deliver("evt_refund_failed", "refund.failed", &refund)
        .await;
    scene
        .deliver("evt_refund_updated", "refund.updated", &refund)
        .await;
    assert_eq!(scene.balance().await, 20 * DOLLAR);
    assert_eq!(scene.audited("wallet.refund_reversed").await, 1);

    // Spent down, then refunded in full: the balance goes below zero, because
    // the money already left, and the organization is on the free plan.
    scene.spend(19 * DOLLAR, "spent").await.expect("covered");
    scene.mock.refund("re_2", "pi_refunded", 2_000, "succeeded");
    scene.deliver("evt_full", "charge.refunded", &charge).await;
    assert_eq!(scene.balance().await, -19 * DOLLAR);
    assert_eq!(scene.balance().await, scene.ledger_sum().await);
    let (_status, body) = scene.billing_body(&scene.owner).await;
    assert_eq!(body["plan"]["key"], "free");
    assert!(
        scene
            .spend(1, "after")
            .await
            .expect_err("owes")
            .is_insufficient()
    );
}

#[tokio::test]
async fn reconciliation_credits_a_top_up_whose_events_never_arrived() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    scene.mock.pay(
        "pi_missed",
        &scene.customer,
        &scene.wallet_metadata("top_up"),
        1_500,
        &json!({}),
    );
    // A payment that is not the wallet's is left alone.
    scene
        .mock
        .pay("pi_invoice", &scene.customer, &json!({}), 2_900, &json!({}));

    let (status, body) = scene
        .request("POST", "/reconcile", None, &scene.owner)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["wallet"]["balance_micros"], 15 * DOLLAR);
    scene
        .request("POST", "/reconcile", None, &scene.owner)
        .await;
    assert_eq!(scene.entries().await, [("top_up".to_owned(), 15 * DOLLAR)]);
}

#[tokio::test]
async fn only_the_billing_roles_spend_or_read_the_wallet_and_other_tenants_see_nothing() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::new(&database).await;
    scene.top_up("pi_seed", 1_000, true).await;

    let member = scene.plain_member().await;
    let (status, body) = scene.billing_body(&member).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["plan"]["key"], "member",
        "the plan explains itself to everyone"
    );
    assert_eq!(
        body["wallet"],
        Value::Null,
        "the balance is the billing roles' business"
    );
    for (method, path, payload) in [
        ("GET", "/wallet/entries", None),
        (
            "POST",
            "/wallet/top-up",
            Some(json!({ "amount_micros": 10 * DOLLAR })),
        ),
        (
            "PUT",
            "/wallet/auto-recharge",
            Some(json!({ "threshold_micros": 0, "amount_micros": 10 * DOLLAR })),
        ),
        ("DELETE", "/wallet/auto-recharge", None),
    ] {
        let (status, body) = scene.request(method, path, payload, &member).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }

    let stranger = register(
        &scene.harness.router,
        &format!("stranger-{}@example.com", Uuid::new_v4()),
    )
    .await;
    for (method, path) in [
        ("GET", ""),
        ("GET", "/wallet/entries"),
        ("DELETE", "/wallet/auto-recharge"),
    ] {
        let (status, _body) = scene.request(method, path, None, &stranger).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} answers as nonexistent"
        );
    }

    // Turning auto-recharge on needs a saved card; this one has one, and the
    // change is audited to the person who made it.
    scene.enable_auto_recharge().await;
    let (status, body) = scene
        .request("DELETE", "/wallet/auto-recharge", None, &scene.owner)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["auto_recharge"], Value::Null);
    assert_eq!(scene.audited("wallet.auto_recharge_changed").await, 2);
    let mut connection = scene.connection().await;
    let actor: Option<String> = audit_events::table
        .filter(audit_events::action.eq("wallet.auto_recharge_changed"))
        .filter(audit_events::organization_id.eq(scene.organization_id))
        .select(audit_events::actor_name)
        .first(&mut connection)
        .await
        .expect("the audit row must read");
    assert!(actor.is_some_and(|name| name.starts_with("owner-")));
}

#[tokio::test]
async fn without_stripe_the_wallet_reads_and_every_write_says_so() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    let scene = Scene::with_stripe(&database, None, Mock::default()).await;

    let (status, body) = scene.billing_body(&scene.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["billing_enabled"], false);
    assert_eq!(body["wallet"]["balance_micros"], 0);
    for (method, path, payload) in [
        (
            "POST",
            "/wallet/top-up",
            Some(json!({ "amount_micros": 10 * DOLLAR })),
        ),
        (
            "PUT",
            "/wallet/auto-recharge",
            Some(json!({ "threshold_micros": 0, "amount_micros": 10 * DOLLAR })),
        ),
    ] {
        let (status, body) = scene.request(method, path, payload, &scene.owner).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {path}: {body}"
        );
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("STRIPE_SECRET_KEY"))
        );
    }
    let (status, _body) = scene
        .request("GET", "/wallet/entries", None, &scene.owner)
        .await;
    assert_eq!(status, StatusCode::OK);

    // Credit already held is still spent: metering does not stop because the
    // deployment cannot sell more for a while.
    // Auto-recharge was set up while Stripe was configured; with nothing able
    // to recharge now, crossing the low mark tells the people instead.
    let mut connection = scene.connection().await;
    diesel::insert_into(wallets::table)
        .values((
            wallets::organization_id.eq(scene.organization_id),
            wallets::balance_micros.eq(5 * DOLLAR),
            wallets::recharge_payment_method_id.eq("pm_card_visa"),
            wallets::auto_recharge_threshold_micros.eq(4 * DOLLAR),
            wallets::auto_recharge_amount_micros.eq(10 * DOLLAR),
        ))
        .execute(&mut connection)
        .await
        .expect("the wallet must insert");
    let server = Subject::new("Server", Uuid::new_v4());
    let mut connection = scene.connection().await;
    let spent = scene
        .wallets
        .spend(
            &mut connection,
            scene.organization_id,
            Spend {
                amount_micros: DOLLAR,
                key: "metered",
                description: None,
                subject: Some(&server),
            },
        )
        .await
        .expect("held credit is spent");
    assert_eq!(spent.balance_micros(), 4 * DOLLAR);
    assert_eq!(spent.entry().subject_id, Some(server.id()));

    scene
        .spend(3 * DOLLAR, "metered-2")
        .await
        .expect("held credit is spent");
    assert_eq!(
        scene.recharges_queued().await,
        0,
        "nothing can charge the card"
    );
    assert_eq!(scene.notified("wallet.low_balance").await, 1);
}

#[tokio::test]
async fn a_top_up_while_stripe_is_unreachable_answers_unavailable() {
    let Some(database) = TestDatabase::create("billing_wallet_flow").await else {
        return;
    };
    // A port nothing listens on: every call to Stripe fails to connect.
    let unreachable = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a loopback port");
    let base = format!("http://{unreachable}");
    let scene = Scene::with_stripe(&database, Some(&base), Mock::default()).await;

    let (status, body) = scene
        .request(
            "POST",
            "/wallet/top-up",
            Some(json!({ "amount_micros": 10 * DOLLAR })),
            &scene.owner,
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert!(scene.entries().await.is_empty(), "nothing is credited");
}
