//! Plan limits end to end: seats refused, soft limits reported, seats billed.
//!
//! Three things that only mean something against a real database and a real
//! Stripe, so this suite brings both: `TestDatabase` for the first, and the
//! same mock Stripe `billing_flow.rs` uses for the second.
//!
//! - The `seats` limit is the one the framework enforces itself, at the one
//!   place a person joins an organization. An invitation past it is refused
//!   with a message naming the plan, and re-inviting somebody who already holds
//!   a seat is free.
//! - A soft limit never refuses. Both halves are checked against a real
//!   subscription row, so plan resolution is exercised rather than assumed.
//! - A per-seat price is kept current: a membership change queues
//!   `SyncSeats`, and running it tells Stripe the new quantity exactly once.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use anubis::billing::{Limits, PlanSet, Reconciler, SyncSeats};
use anubis::config::AppConfig;
use anubis::schema::{jobs, subscriptions};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use support::{Harness, TestDatabase, register, send};
use uuid::Uuid;

/// Two plans, both charging per seat, so every path here is live.
const PER_SEAT_YML: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 2
      creative_concepts: 3
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
        per_seat: true
    limits:
      seats: 25
      creative_concepts:
        count: 10
        enforcement: soft
";

/// The same two plans with flat pricing, which sells no seats to Stripe.
const FLAT_YML: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 2
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

const SUBSCRIPTION_ID: &str = "sub_seats";
const ITEM_ID: &str = "si_seats";
const CUSTOMER_ID: &str = "cus_seats";

fn plans(yaml: &str) -> PlanSet {
    PlanSet::from_yaml(yaml).expect("the plans must parse")
}

/// One request the mock Stripe took, kept as it arrived.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    body: String,
    idempotency_key: Option<String>,
}

type Log = Arc<Mutex<Vec<Received>>>;

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// The quantity the mock currently reports for its one subscription.
type Quantity = Arc<Mutex<i64>>;

#[derive(Clone)]
struct MockState {
    log: Log,
    quantity: Quantity,
    /// Updates still to refuse with a `500`.
    refusals: Arc<Mutex<u32>>,
    /// Stripe's idempotency cache: the first answer to each key, refusals
    /// included, which Stripe replays to every retry with that key.
    answered: Arc<Mutex<BTreeMap<String, (StatusCode, Value)>>>,
    /// The subscription's status, which a test moves to `canceled`.
    status: Arc<Mutex<String>>,
    /// Whether the next update's answer is lost on its way back: Stripe applies
    /// and saves it, and the client reads garbage.
    lose_next_answer: Arc<Mutex<bool>>,
}

/// Serves the two subscription endpoints seat synchronization calls.
///
/// The mock is stateful on purpose: an update sets the quantity it answers
/// with afterwards, which is what makes running the job twice provably a no-op
/// rather than an assertion about call counts alone.
async fn start_mock_stripe() -> (String, Log, Quantity) {
    let (base, state) = start_mock_stripe_refusing(0).await;
    (base, state.log, state.quantity)
}

/// The same mock, refusing its first `refusals` updates with a `500` that it
/// saves under the request's idempotency key, as Stripe does.
async fn start_mock_stripe_refusing(refusals: u32) -> (String, MockState) {
    let state = MockState {
        log: Arc::new(Mutex::new(Vec::new())),
        quantity: Arc::new(Mutex::new(1)),
        refusals: Arc::new(Mutex::new(refusals)),
        answered: Arc::new(Mutex::new(BTreeMap::new())),
        status: Arc::new(Mutex::new("active".to_owned())),
        lose_next_answer: Arc::new(Mutex::new(false)),
    };

    let router = Router::new()
        .route(
            "/v1/subscriptions/{id}",
            get(
                async |State(state): State<MockState>, Path(id): Path<String>| {
                    unlock(&state.log).push(Received {
                        path: format!("GET /v1/subscriptions/{id}"),
                        body: String::new(),
                        idempotency_key: None,
                    });
                    Json(subscription_body(
                        *unlock(&state.quantity),
                        &unlock(&state.status),
                    ))
                },
            )
            .post(
                async |State(state): State<MockState>,
                       Path(id): Path<String>,
                       headers: HeaderMap,
                       body: String| {
                    let key = headers
                        .get("idempotency-key")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    unlock(&state.log).push(Received {
                        path: format!("POST /v1/subscriptions/{id}"),
                        body: body.clone(),
                        idempotency_key: key.clone(),
                    });
                    if let Some(saved) = key
                        .as_ref()
                        .and_then(|key| unlock(&state.answered).get(key).cloned())
                    {
                        // Stripe marks an answer it saved for an earlier request.
                        return (saved.0, [("idempotent-replayed", "true")], Json(saved.1))
                            .into_response();
                    }

                    let answer = if *unlock(&state.refusals) > 0 {
                        *unlock(&state.refusals) -= 1;
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            json!({ "error": { "type": "api_error",
                                               "message": "An unknown error occurred" } }),
                        )
                    } else {
                        if let Some(quantity) = quantity_of(&body) {
                            *unlock(&state.quantity) = quantity;
                        }
                        (
                            StatusCode::OK,
                            subscription_body(*unlock(&state.quantity), &unlock(&state.status)),
                        )
                    };
                    if let Some(key) = key {
                        unlock(&state.answered).insert(key, answer.clone());
                    }
                    if std::mem::take(&mut *unlock(&state.lose_next_answer)) {
                        return (StatusCode::OK, "<the connection dropped mid-answer")
                            .into_response();
                    }
                    (answer.0, Json(answer.1)).into_response()
                },
            ),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the mock must bind a loopback port");
    let address = listener
        .local_addr()
        .expect("the mock must report its address");
    tokio::spawn(async move {
        let _served = axum::serve(listener, router).await;
    });

    (format!("http://{address}"), state)
}

/// Stripe's subscription document, as much of one as the framework reads.
fn subscription_body(quantity: i64, status: &str) -> Value {
    json!({
        "id": SUBSCRIPTION_ID,
        "object": "subscription",
        "status": status,
        "customer": CUSTOMER_ID,
        "cancel_at_period_end": false,
        "items": { "object": "list", "data": [{
            "id": ITEM_ID,
            "quantity": quantity,
            "current_period_end": 1_800_000_000_i64,
            "price": { "id": "price_pro_monthly", "recurring": { "interval": "month" } },
        }] },
    })
}

/// Reads `items[0][quantity]` back out of a form-encoded update.
fn quantity_of(body: &str) -> Option<i64> {
    url::form_urlencoded::parse(body.as_bytes())
        .find(|(name, _value)| name == "items[0][quantity]")
        .and_then(|(_name, value)| value.parse().ok())
}

/// The application's config, pointed at the mock Stripe.
fn config(api_base: &str) -> AppConfig {
    let api_base = api_base.to_owned();
    AppConfig::from_lookup(move |name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some("https://app.example.com".to_owned()),
        "STRIPE_SECRET_KEY" => Some("sk_test_limits_flow".to_owned()),
        "STRIPE_API_BASE" => Some(api_base.clone()),
        _other => None,
    })
    .expect("the test config must parse")
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

/// Invites an address into the organization, answering with the status and body.
async fn invite(
    router: &Router,
    organization_id: Uuid,
    cookie: &str,
    email: &str,
) -> (StatusCode, Value) {
    let body = json!({ "email": email, "organization_id": organization_id, "roles": [] });
    let (status, _headers, answer) = send(
        router,
        "POST",
        "/tenancy/invitations",
        Some(&body),
        Some(cookie),
    )
    .await;
    (status, answer)
}

/// How many seat-synchronizing jobs are waiting.
async fn queued_seat_syncs(pool: &anubis::db::DbPool, organization_id: Uuid) -> i64 {
    let mut connection = pool.get().await.expect("a connection must be available");
    jobs::table
        .filter(jobs::kind.eq("anubis.billing.sync_seats"))
        .filter(jobs::queue.eq("billing"))
        .filter(jobs::payload.eq(json!({ "organization_id": organization_id })))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the job count must read")
}

/// Writes the live subscription an organization would hold after checkout.
async fn insert_subscription(pool: &anubis::db::DbPool, organization_id: Uuid, quantity: i32) {
    let mut connection = pool.get().await.expect("a connection must be available");
    diesel::update(anubis::schema::organizations::table.find(organization_id))
        .set(anubis::schema::organizations::stripe_customer_id.eq(CUSTOMER_ID))
        .execute(&mut connection)
        .await
        .expect("the customer must store");
    diesel::insert_into(subscriptions::table)
        .values((
            subscriptions::organization_id.eq(organization_id),
            subscriptions::plan_key.eq("pro"),
            subscriptions::stripe_subscription_id.eq(SUBSCRIPTION_ID),
            subscriptions::status.eq("active"),
            subscriptions::billing_interval.eq("monthly"),
            subscriptions::quantity.eq(quantity),
        ))
        .execute(&mut connection)
        .await
        .expect("the subscription must insert");
}

#[tokio::test]
async fn the_seats_limit_refuses_the_invitation_that_would_break_it() {
    let Some(database) = TestDatabase::create("billing_limits_seats").await else {
        return;
    };
    let harness = Harness::boot_with_plans(&database, plans(PER_SEAT_YML)).await;

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;

    // The owner is the first seat, so the free plan's two leave room for one.
    let first = format!("first-{}@example.com", Uuid::new_v4());
    let (status, body) = invite(&harness.router, organization_id, &owner, &first).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let second = format!("second-{}@example.com", Uuid::new_v4());
    let (status, body) = invite(&harness.router, organization_id, &owner, &second).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the third seat is refused: body: {body}",
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert!(message.contains("Free"), "the plan is named: {message}");
    assert!(message.contains("seats"), "the limit is named: {message}");
    assert!(
        message.contains("Upgrade"),
        "the way out is named: {message}"
    );

    // Re-inviting somebody who already holds a seat costs nothing, because the
    // count is of people rather than of invitations.
    let (status, body) = invite(&harness.router, organization_id, &owner, &first).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let mut connection = harness
        .pool
        .get()
        .await
        .expect("a connection must be available");
    let limits = Limits::new(plans(PER_SEAT_YML));
    assert_eq!(
        limits
            .seats_used(&mut connection, organization_id)
            .await
            .expect("the seats must count"),
        2,
        "one owner and one invitation, counted once each",
    );
}

#[tokio::test]
async fn a_soft_limit_reports_where_a_hard_one_refuses() {
    let Some(database) = TestDatabase::create("billing_limits_soft").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let limits = Limits::new(plans(PER_SEAT_YML));

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    let mut connection = harness
        .pool
        .get()
        .await
        .expect("a connection must be available");

    // On free, which limits creative concepts to three, hard.
    limits
        .check(&mut connection, organization_id, "creative_concepts", 2)
        .await
        .expect("the third concept fits");
    let refused = limits
        .check(&mut connection, organization_id, "creative_concepts", 3)
        .await
        .expect_err("the fourth concept is refused");
    assert!(refused.is_exceeded(), "got: {refused}");

    // The same organization on pro, where the limit is ten and soft.
    insert_subscription(&harness.pool, organization_id, 1).await;
    limits
        .check(&mut connection, organization_id, "creative_concepts", 500)
        .await
        .expect("a soft limit lets the record through and leaves the screen to warn");
    limits
        .check(&mut connection, organization_id, "anything_unnamed", 10_000)
        .await
        .expect("a limit the plan does not name is unlimited");
}

#[tokio::test]
async fn a_membership_change_queues_a_seat_sync_only_when_seats_are_sold() {
    let Some(database) = TestDatabase::create("billing_limits_queue").await else {
        return;
    };

    // Flat pricing first: nothing at Stripe moves when a person joins.
    let flat = Harness::boot_with_plans(&database, plans(FLAT_YML)).await;
    let owner = register(
        &flat.router,
        &format!("flat-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&flat.router, &owner).await;
    let (status, body) = invite(
        &flat.router,
        organization_id,
        &owner,
        &format!("guest-{}@example.com", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        queued_seat_syncs(&flat.pool, organization_id).await,
        0,
        "a flat price has no quantity for a membership to move",
    );

    // Per-seat pricing: the same act queues the update.
    let per_seat = Harness::boot_with_plans(&database, plans(PER_SEAT_YML)).await;
    let owner = register(
        &per_seat.router,
        &format!("seats-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&per_seat.router, &owner).await;
    let guest = format!("guest-{}@example.com", Uuid::new_v4());
    let (status, body) = invite(&per_seat.router, organization_id, &owner, &guest).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        queued_seat_syncs(&per_seat.pool, organization_id).await,
        1,
        "the invitation queued the seat count Stripe has to be told",
    );

    // And so does taking it back.
    let (status, _headers, roster) = send(
        &per_seat.router,
        "GET",
        &format!("/tenancy/organizations/{organization_id}/members"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {roster}");
    let invitation_id = roster["members"]
        .as_array()
        .and_then(|members| {
            members
                .iter()
                .find_map(|member| member["invitation_id"].as_str())
        })
        .expect("the pending invitation must be on the roster");

    let (status, _headers, body) = send(
        &per_seat.router,
        "DELETE",
        &format!("/tenancy/organizations/{organization_id}/invitations/{invitation_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(
        queued_seat_syncs(&per_seat.pool, organization_id).await,
        2,
        "a revoked invitation releases a seat, which Stripe also has to be told",
    );
}

#[tokio::test]
async fn seat_synchronization_tells_stripe_the_new_quantity_once() {
    let Some(database) = TestDatabase::create("billing_limits_sync").await else {
        return;
    };
    let harness = Harness::boot_with_plans(&database, plans(PER_SEAT_YML)).await;
    let (stripe_base, stripe_log, stripe_quantity) = start_mock_stripe().await;
    let reconciler = Reconciler::new(
        harness.pool.clone(),
        plans(PER_SEAT_YML),
        &config(&stripe_base),
    );

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    // Bought for one seat, and then somebody was invited.
    insert_subscription(&harness.pool, organization_id, 1).await;
    let (status, body) = invite(
        &harness.router,
        organization_id,
        &owner,
        &format!("guest-{}@example.com", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect("the seat sync must succeed");

    let updates: Vec<Received> = unlock(&stripe_log)
        .iter()
        .filter(|call| call.path.starts_with("POST"))
        .cloned()
        .collect();
    assert_eq!(updates.len(), 1, "one update: {:?}", unlock(&stripe_log));
    assert_eq!(
        updates[0].path,
        format!("POST /v1/subscriptions/{SUBSCRIPTION_ID}"),
    );
    assert!(
        updates[0].body.contains("items%5B0%5D%5Bid%5D=si_seats"),
        "the line item is named: {}",
        updates[0].body,
    );
    assert_eq!(
        quantity_of(&updates[0].body),
        Some(2),
        "the owner and the invitation are two seats: {}",
        updates[0].body,
    );
    assert_eq!(*unlock(&stripe_quantity), 2, "Stripe holds the new number");

    let mut connection = harness
        .pool
        .get()
        .await
        .expect("a connection must be available");
    let stored: i32 = subscriptions::table
        .filter(subscriptions::stripe_subscription_id.eq(SUBSCRIPTION_ID))
        .select(subscriptions::quantity)
        .first(&mut connection)
        .await
        .expect("the subscription row must read");
    assert_eq!(stored, 2, "the projection agrees with the invoice");

    // Running it again sends nothing: the quantity is already right, which is
    // what makes an at-least-once queue safe here.
    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect("a second run must succeed");
    let updates = unlock(&stripe_log)
        .iter()
        .filter(|call| call.path.starts_with("POST"))
        .count();
    assert_eq!(updates, 1, "a correct quantity is never re-sent");
}

/// A seat update Stripe refused is retried under a fresh key, not replayed.
///
/// Stripe saves the first answer to an idempotency key, a `500` included, and
/// replays it to every retry with that key for a day. The key names the
/// subscription row's last write, so the refusal moves it: the job's next
/// attempt is a new request, which Stripe can answer afresh.
#[tokio::test]
async fn a_seat_update_stripe_refused_is_sent_afresh_on_the_next_attempt() {
    let Some(database) = TestDatabase::create("billing_limits_sync").await else {
        return;
    };
    let harness = Harness::boot_with_plans(&database, plans(PER_SEAT_YML)).await;
    let (stripe_base, stripe) = start_mock_stripe_refusing(1).await;
    let (stripe_log, stripe_quantity) = (stripe.log, stripe.quantity);
    let reconciler = Reconciler::new(
        harness.pool.clone(),
        plans(PER_SEAT_YML),
        &config(&stripe_base),
    );

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    insert_subscription(&harness.pool, organization_id, 1).await;
    let (status, body) = invite(
        &harness.router,
        organization_id,
        &owner,
        &format!("guest-{}@example.com", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect_err("Stripe refuses the first attempt");
    assert_eq!(*unlock(&stripe_quantity), 1, "nothing changed at Stripe");

    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect("the retry is a new request, and Stripe takes it");
    assert_eq!(*unlock(&stripe_quantity), 2);

    let keys: Vec<Option<String>> = unlock(&stripe_log)
        .iter()
        .filter(|call| call.path.starts_with("POST"))
        .map(|call| call.idempotency_key.clone())
        .collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(
        keys.iter().all(Option::is_some),
        "every update is keyed: {keys:?}"
    );
    assert_ne!(keys[0], keys[1], "the refusal moved the key");
}

/// A seat update whose answer was lost is not sent again once Stripe has moved on.
///
/// The first attempt sets the seats at Stripe and its answer never arrives;
/// before the job's retry, the subscription is cancelled. The retry reads
/// Stripe, finds it over, and writes that, rather than resending the update
/// and receiving the first attempt's saved answer, which still says active.
#[tokio::test]
async fn a_seat_update_whose_answer_was_lost_is_not_sent_again() {
    let Some(database) = TestDatabase::create("billing_limits_sync").await else {
        return;
    };
    let harness = Harness::boot_with_plans(&database, plans(PER_SEAT_YML)).await;
    let (stripe_base, stripe) = start_mock_stripe_refusing(0).await;
    let reconciler = Reconciler::new(
        harness.pool.clone(),
        plans(PER_SEAT_YML),
        &config(&stripe_base),
    );

    let owner = register(
        &harness.router,
        &format!("owner-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let organization_id = bootstrapped_organization(&harness.router, &owner).await;
    insert_subscription(&harness.pool, organization_id, 1).await;
    let (status, body) = invite(
        &harness.router,
        organization_id,
        &owner,
        &format!("guest-{}@example.com", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    *unlock(&stripe.lose_next_answer) = true;
    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect_err("the answer never arrived");
    assert_eq!(
        *unlock(&stripe.quantity),
        2,
        "Stripe applied it all the same"
    );

    *unlock(&stripe.status) = "canceled".to_owned();
    reconciler
        .sync_seats(SyncSeats { organization_id })
        .await
        .expect("the retry finds the subscription over, and records that");

    let updates = unlock(&stripe.log)
        .iter()
        .filter(|call| call.path.starts_with("POST"))
        .count();
    assert_eq!(updates, 1, "the update was not sent a second time");
    let mut connection = harness
        .pool
        .get()
        .await
        .expect("a connection must be available");
    let recorded: String = subscriptions::table
        .filter(subscriptions::stripe_subscription_id.eq(SUBSCRIPTION_ID))
        .select(subscriptions::status)
        .first(&mut connection)
        .await
        .expect("the subscription row must read");
    assert_eq!(
        recorded, "canceled",
        "the projection holds what Stripe holds"
    );
}
