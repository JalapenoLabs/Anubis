//! Outgoing webhooks end to end: subscribe, emit, deliver, retry, redeliver.
//!
//! The receiver is a real HTTP server on loopback, so nothing about the
//! delivery path is stubbed: the framework resolves the endpoint, opens its
//! sealed signing secret, signs the exact bytes it sends, and posts them over a
//! socket. The test then verifies that signature the way a customer's receiver
//! would, against the secret it was handed once at subscription time.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anubis::jobs::{Job, Worker};
use anubis::schema::{team_memberships, teams, users, webhook_deliveries};
use anubis::webhooks::{self, DeliverWebhook, signature};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// Minimal roles: this test authorizes on the admin key, not on models.
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
    models: {}
";

/// How long a condition driven by the worker has to come true.
///
/// Generous, because the whole path is real: a claim, an HTTP round trip, and
/// a write back. The test tracks the worker rather than the clock, so a fast
/// machine never waits this long.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

/// One request the receiver took, kept exactly as it arrived.
#[derive(Debug, Clone)]
struct Received {
    headers: HeaderMap,
    body: String,
}

type Log = Arc<Mutex<Vec<Received>>>;

fn unlock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().expect("the test lock must not be poisoned")
}

/// A receiver that accepts on `/hooks/ok` and refuses on `/hooks/fail`.
///
/// Returns its base URL and the log of everything the accepting route took.
async fn start_receiver() -> (String, Log) {
    let accepted: Log = Arc::new(Mutex::new(Vec::new()));

    let app = Router::new()
        .route(
            "/hooks/ok",
            post(
                async |State(log): State<Log>, headers: HeaderMap, body: String| {
                    unlock(&log).push(Received { headers, body });
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/hooks/fail",
            post(async || StatusCode::INTERNAL_SERVER_ERROR),
        )
        .with_state(Arc::clone(&accepted));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the receiver must bind a loopback port");
    let address = listener
        .local_addr()
        .expect("the receiver must report its address");
    tokio::spawn(async move {
        let _served = axum::serve(listener, app).await;
    });

    (format!("http://{address}"), accepted)
}

/// Sends one JSON request to the application under test.
async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }

    let built = match body {
        Some(value) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body must serialize"),
            )),
        None => builder.body(Body::empty()),
    }
    .expect("request must build");

    let response = router
        .clone()
        .oneshot(built)
        .await
        .expect("request must complete");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn session_token(headers: &HeaderMap) -> String {
    for value in headers.get_all(SET_COOKIE) {
        if let Some(rest) = value
            .to_str()
            .ok()
            .and_then(|rendered| rendered.strip_prefix("anubis_session="))
        {
            let token = rest.split(';').next().unwrap_or_default();
            if !token.is_empty() {
                return token.to_owned();
            }
        }
    }
    panic!("no session cookie in response");
}

/// Registers an account and returns its session cookie.
async fn register(router: &Router, email: &str) -> String {
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/register",
        Some(&json!({ "email": email, "password": "correct horse battery staple" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// The delivery row's state, which is the whole debugging story.
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = webhook_deliveries, check_for_backend(diesel::pg::Pg))]
struct DeliveryRow {
    id: Uuid,
    status: String,
    attempts: i32,
    response_status: Option<i32>,
    last_error: Option<String>,
    delivered_at: Option<DateTime<Utc>>,
}

/// Every delivery an endpoint has, oldest first.
async fn deliveries(connection: &mut AsyncPgConnection, endpoint_id: Uuid) -> Vec<DeliveryRow> {
    webhook_deliveries::table
        .filter(webhook_deliveries::webhook_endpoint_id.eq(endpoint_id))
        .order(webhook_deliveries::created_at.asc())
        .select(DeliveryRow::as_select())
        .load(connection)
        .await
        .expect("the delivery query must succeed")
}

/// Polls until every delivery of `endpoint_id` has stopped moving.
///
/// A row is at rest once it is `delivered` or `dead`, or once it has failed at
/// least once, which is as far as one worker pass takes a retryable failure.
async fn settle(connection: &mut AsyncPgConnection, endpoint_id: Uuid, expected: usize) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let rows = deliveries(connection, endpoint_id).await;
        let at_rest = rows.len() == expected
            && rows
                .iter()
                .all(|row| row.status != "pending" && row.attempts > 0);
        if at_rest {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never settled {endpoint_id}: {rows:?}",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear narrative: a single worker run is what every assertion observes"
)]
async fn events_reach_a_real_receiver_signed_retried_and_redelivered() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping webhooks_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");
    let mut connection = pool.get().await.expect("a connection must be available");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let roles = anubis::roles::RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();

    let router = Router::new()
        .nest("/auth", anubis::auth::router(pool.clone(), mailer, &config))
        .nest(
            "/developers",
            webhooks::router(pool.clone(), roles, &config),
        );

    let (receiver_url, accepted) = start_receiver().await;

    let run = Uuid::new_v4();
    let admin_cookie = register(&router, &format!("webhooks-admin-{run}@example.com")).await;
    let outsider_cookie = register(&router, &format!("webhooks-outsider-{run}@example.com")).await;

    let admin_id: Uuid = users::table
        .filter(users::email.eq(format!("webhooks-admin-{run}@example.com")))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the admin must exist");
    let team_id: Uuid = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(admin_id))),
        )
        .select(teams::id)
        .first(&mut connection)
        .await
        .expect("the bootstrapped team must exist");
    let endpoints_path = format!("/developers/teams/{team_id}/webhook-endpoints");

    // ------------------------------------------------------------------
    // Subscribing. The secret is handed over once and never again.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        "POST",
        &endpoints_path,
        Some(&json!({
            "url": format!("{receiver_url}/hooks/ok"),
            "description": "Accepting receiver",
            "event_types": ["widget.created"],
        })),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let secret = body["secret"]
        .as_str()
        .expect("the secret is returned once")
        .to_owned();
    assert!(secret.starts_with("whsec_"), "got: {secret}");
    let accepting_id: Uuid = body["webhook_endpoint"]["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("the response carries the endpoint");

    let (status, _headers, body) = send(
        &router,
        "POST",
        &endpoints_path,
        Some(&json!({
            "url": format!("{receiver_url}/hooks/fail"),
            "event_types": ["widget.destroyed"],
        })),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let refusing_id: Uuid = body["webhook_endpoint"]["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("the response carries the endpoint");

    // Listing never carries the secret back.
    let (status, _headers, body) =
        send(&router, "GET", &endpoints_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !body.to_string().contains(&secret),
        "the signing secret must never be listed",
    );

    // A typo in the event type is refused where it was typed.
    let (status, _headers, _body) = send(
        &router,
        "POST",
        &endpoints_path,
        Some(&json!({
            "url": format!("{receiver_url}/hooks/ok"),
            "event_types": ["widget.create"],
        })),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ------------------------------------------------------------------
    // Emitting. A rolled-back write takes its event with it, which is the
    // whole reason the queue lives in Postgres.
    // ------------------------------------------------------------------
    let payload = json!({ "id": run.to_string(), "name": "Widget" });

    connection
        .transaction::<(), diesel::result::Error, _>(async |connection| {
            webhooks::emit(connection, team_id, "widget.created", &payload).await?;
            Err(diesel::result::Error::RollbackTransaction)
        })
        .await
        .expect_err("the transaction must roll back");
    assert!(
        deliveries(&mut connection, accepting_id).await.is_empty(),
        "a rolled-back write must leave no delivery behind",
    );

    // An event nobody subscribed to costs one query and nothing else.
    let notified = webhooks::emit(&mut connection, team_id, "widget.updated", &payload)
        .await
        .expect("emitting must succeed");
    assert_eq!(notified, 0);

    let notified = webhooks::emit(&mut connection, team_id, "widget.created", &payload)
        .await
        .expect("emitting must succeed");
    assert_eq!(notified, 1, "one active endpoint wants this event");

    // ------------------------------------------------------------------
    // Delivering, through the framework's own job on a real worker.
    // ------------------------------------------------------------------
    let deliverer = webhooks::Deliverer::new(pool.clone(), &config);
    let worker = Worker::builder(pool.clone())
        .register(move |job: DeliverWebhook| {
            let deliverer = deliverer.clone();
            async move { deliverer.deliver(job).await }
        })
        // Fast enough that the test tracks the worker rather than the clock.
        .poll_interval(Duration::from_millis(25))
        .build();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(worker.run(async move {
        let _stopped = stopped.await;
    }));

    settle(&mut connection, accepting_id, 1).await;

    let settled = deliveries(&mut connection, accepting_id).await;
    assert_eq!(settled[0].status, "delivered", "{settled:?}");
    assert_eq!(settled[0].attempts, 1);
    assert_eq!(settled[0].response_status, Some(200));
    assert!(settled[0].last_error.is_none(), "{settled:?}");
    assert!(settled[0].delivered_at.is_some(), "{settled:?}");

    // ------------------------------------------------------------------
    // The request itself: the headers a receiver routes on, and a signature
    // that verifies against the secret handed over at subscription time.
    // ------------------------------------------------------------------
    let request = {
        let taken = unlock(&accepted);
        assert_eq!(taken.len(), 1, "the receiver must have taken one request");
        taken[0].clone()
    };
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };

    assert_eq!(header(CONTENT_TYPE.as_str()), "application/json");
    assert_eq!(header(signature::EVENT_HEADER), "widget.created");
    assert_eq!(header(signature::ID_HEADER), settled[0].id.to_string());
    assert_eq!(
        serde_json::from_str::<Value>(&request.body).expect("the body must be JSON"),
        payload,
        "the payload is the serialized record, verbatim",
    );

    let timestamp: i64 = header(signature::TIMESTAMP_HEADER)
        .parse()
        .expect("the timestamp header must be unix seconds");
    assert!(
        signature::verify(
            &secret,
            timestamp,
            &header(signature::SIGNATURE_HEADER),
            &request.body,
            Utc::now().timestamp(),
        ),
        "the receiver must be able to verify the signature: {request:?}",
    );
    assert!(
        !signature::verify(
            "whsec_not-the-secret",
            timestamp,
            &header(signature::SIGNATURE_HEADER),
            &request.body,
            Utc::now().timestamp(),
        ),
        "and only with the right secret",
    );

    // ------------------------------------------------------------------
    // A receiver that refuses. The first failure retries; exhausting the
    // attempts buries the delivery where the team can see it.
    // ------------------------------------------------------------------
    let notified = webhooks::emit(&mut connection, team_id, "widget.destroyed", &payload)
        .await
        .expect("emitting must succeed");
    assert_eq!(notified, 1);

    settle(&mut connection, refusing_id, 1).await;
    let failed = deliveries(&mut connection, refusing_id).await;
    assert_eq!(failed[0].status, "failed", "{failed:?}");
    assert_eq!(failed[0].attempts, 1);
    assert_eq!(failed[0].response_status, Some(500));
    assert!(
        failed[0]
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("500")),
        "the row says what came back: {failed:?}",
    );

    // The queue's backoff would take minutes to reach the last attempt, so the
    // row is moved to the edge of its budget and given one more job. What is
    // under test is the boundary, not the arithmetic behind it, which
    // `jobs_flow` already proves.
    diesel::update(webhook_deliveries::table.find(failed[0].id))
        .set(webhook_deliveries::attempts.eq(DeliverWebhook::MAX_ATTEMPTS - 1))
        .execute(&mut connection)
        .await
        .expect("the delivery must be movable");
    anubis::jobs::enqueue(
        &mut connection,
        &DeliverWebhook {
            delivery_id: failed[0].id,
        },
    )
    .await
    .expect("enqueueing must succeed");

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let rows = deliveries(&mut connection, refusing_id).await;
        if rows[0].status == "dead" {
            assert_eq!(rows[0].attempts, DeliverWebhook::MAX_ATTEMPTS);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an exhausted delivery must be buried: {rows:?}",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // ------------------------------------------------------------------
    // Redelivering leaves the first attempt's history alone and sends again.
    // ------------------------------------------------------------------
    let redeliver_path = format!(
        "/developers/teams/{team_id}/webhook-endpoints/{accepting_id}/deliveries/{}/redeliver",
        settled[0].id,
    );
    let (status, _headers, body) =
        send(&router, "POST", &redeliver_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["webhook_delivery"]["status"], json!("pending"));
    assert_eq!(
        body["webhook_delivery"]["event_type"],
        json!("widget.created")
    );

    settle(&mut connection, accepting_id, 2).await;
    assert_eq!(
        unlock(&accepted).len(),
        2,
        "the receiver takes the redelivered request too",
    );

    // ------------------------------------------------------------------
    // A paused endpoint keeps its history and receives nothing new.
    // ------------------------------------------------------------------
    let endpoint_path = format!("{endpoints_path}/{accepting_id}");
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &endpoint_path,
        Some(&json!({ "active": false })),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["webhook_endpoint"]["active"], json!(false));

    let notified = webhooks::emit(&mut connection, team_id, "widget.created", &payload)
        .await
        .expect("emitting must succeed");
    assert_eq!(notified, 0, "a paused endpoint is not a subscriber");

    // ------------------------------------------------------------------
    // Another tenant sees none of it: absent and forbidden look identical.
    // ------------------------------------------------------------------
    let deliveries_path = format!("{endpoint_path}/deliveries");
    for path in [endpoints_path.as_str(), deliveries_path.as_str()] {
        let (status, _headers, _body) =
            send(&router, "GET", path, None, Some(&outsider_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }

    let resume = json!({ "active": true });
    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &endpoint_path,
        Some(&resume),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another tenant may not resume a subscription it cannot see",
    );

    // ------------------------------------------------------------------
    // Deleting the subscription takes its history with it.
    // ------------------------------------------------------------------
    let (status, _headers, _body) =
        send(&router, "DELETE", &endpoint_path, None, Some(&admin_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        deliveries(&mut connection, accepting_id).await.is_empty(),
        "deliveries cascade with the endpoint that owned them",
    );

    stop.send(()).expect("the worker must still be listening");
    running.await.expect("the worker must shut down cleanly");

    // Leave the shared database as this test found it.
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &format!("{endpoints_path}/{refusing_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
