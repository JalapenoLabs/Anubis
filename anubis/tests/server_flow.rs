//! The production serve path, driven the way a deployment runs it.
//!
//! Everything here goes through `anubis::server::harden`, so what is asserted
//! is the stack an application gets from one call to `anubis::server::serve`:
//! request ids, request logging, security headers, the cross-origin posture,
//! the request timeout, and the probes.
//!
//! Only the readiness test with a live database needs `DATABASE_URL`; without
//! it that test logs a skip and passes. Everything else runs against a pool
//! pointed at an address nothing answers on, which is what makes the failure
//! side of readiness testable at all.

use std::time::Duration;

use anubis::config::AppConfig;
use anubis::db::DbPool;
use anubis::server;
use axum::Router;
use axum::body::Body;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_METHOD, CONTENT_SECURITY_POLICY, ORIGIN,
    REFERRER_POLICY, STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

/// The header the framework returns every request's id in.
const X_REQUEST_ID: &str = "x-request-id";

/// An origin a deployment might legitimately allow.
const ALLOWED_ORIGIN: &str = "https://app.example.com";

/// A syntactically valid `ANUBIS_SECRET_KEY`, which production insists on.
const SECRET_KEY: &str = "bkVLZLd1zHBqxWvKGKp5gRTZKcTf9UvHT5vXbHvWJ0M=";

/// A pool that never reaches a database.
///
/// Building one performs no I/O, so it stands in wherever the test cares about
/// composition rather than about data: the probes, the layers, and the failure
/// half of the readiness check.
fn unreachable_pool() -> DbPool {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(
        "postgres://nobody:nobody@127.0.0.1:1/unreachable",
    );
    Pool::builder(manager)
        .build()
        .expect("building a lazy pool never touches the network")
}

/// Configuration from an explicit set of variables, defaults for the rest.
fn config(variables: &[(&str, &str)]) -> AppConfig {
    let owned: Vec<(String, String)> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();

    AppConfig::from_lookup(move |name| {
        owned
            .iter()
            .find(|(variable, _value)| variable == name)
            .map(|(_variable, value)| value.clone())
    })
    .expect("test configuration must parse")
}

/// The hardened application, with one extra route per test need.
fn app(config: &AppConfig, extra: Router) -> Router {
    server::harden(extra, unreachable_pool(), config)
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

async fn send(router: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> Answer {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();

    Answer {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

#[tokio::test]
async fn every_response_carries_a_fresh_request_id() {
    let config = config(&[]);
    let app = app(&config, Router::new());

    let first = send(&app, "GET", "/healthz", &[]).await;
    let second = send(&app, "GET", "/healthz", &[]).await;

    let first_id = first.header(X_REQUEST_ID).expect("an id must come back");
    let second_id = second.header(X_REQUEST_ID).expect("an id must come back");
    assert_eq!(first_id.len(), 36, "got: {first_id}");
    assert_ne!(first_id, second_id, "each request gets its own id");
}

#[tokio::test]
async fn a_caller_cannot_choose_its_own_request_id() {
    let config = config(&[]);
    let app = app(&config, Router::new());

    // The id is the server's correlation handle. A caller that could set it
    // could make two unrelated requests share one line of the log.
    let answer = send(&app, "GET", "/healthz", &[(X_REQUEST_ID, "chosen-by-me")]).await;

    assert_ne!(answer.header(X_REQUEST_ID), Some("chosen-by-me"));
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let config = config(&[]);
    let app = app(&config, Router::new());

    let answer = send(&app, "GET", "/healthz", &[]).await;

    assert_eq!(
        answer.header(X_CONTENT_TYPE_OPTIONS.as_str()),
        Some("nosniff")
    );
    assert_eq!(
        answer.header(REFERRER_POLICY.as_str()),
        Some("strict-origin-when-cross-origin"),
    );
    assert_eq!(answer.header(X_FRAME_OPTIONS.as_str()), Some("DENY"));
    assert_eq!(
        answer.header(CONTENT_SECURITY_POLICY.as_str()),
        Some("frame-ancestors 'none'"),
    );
}

#[tokio::test]
async fn hsts_stays_out_of_development_and_arrives_in_production() {
    // A browser that takes HSTS from localhost refuses plain http to localhost
    // for a year, across every project on that machine.
    let development = app(
        &config(&[("APP_URL", "https://app.example.com")]),
        Router::new(),
    );
    let answer = send(&development, "GET", "/healthz", &[]).await;
    assert_eq!(answer.header(STRICT_TRANSPORT_SECURITY.as_str()), None);

    let production = app(
        &config(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", SECRET_KEY),
            ("APP_URL", "https://app.example.com"),
        ]),
        Router::new(),
    );
    let answer = send(&production, "GET", "/healthz", &[]).await;
    assert_eq!(
        answer.header(STRICT_TRANSPORT_SECURITY.as_str()),
        Some("max-age=31536000; includeSubDomains"),
    );

    // An http APP_URL still gets it on a request a proxy took over https.
    let behind_proxy = app(
        &config(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", SECRET_KEY),
            ("APP_URL", "http://app.example.com"),
        ]),
        Router::new(),
    );
    let plain = send(&behind_proxy, "GET", "/healthz", &[]).await;
    assert_eq!(plain.header(STRICT_TRANSPORT_SECURITY.as_str()), None);
    let forwarded = send(
        &behind_proxy,
        "GET",
        "/healthz",
        &[("x-forwarded-proto", "https")],
    )
    .await;
    assert!(
        forwarded
            .header(STRICT_TRANSPORT_SECURITY.as_str())
            .is_some()
    );
}

#[tokio::test]
async fn no_cors_headers_are_sent_until_origins_are_configured() {
    let config = config(&[]);
    let app = app(&config, Router::new());

    let answer = send(
        &app,
        "GET",
        "/healthz",
        &[(ORIGIN.as_str(), ALLOWED_ORIGIN)],
    )
    .await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.header(ACCESS_CONTROL_ALLOW_ORIGIN.as_str()), None);
}

#[tokio::test]
async fn a_configured_origin_is_allowed_and_every_other_one_is_not() {
    let config = config(&[("CORS_ALLOWED_ORIGINS", ALLOWED_ORIGIN)]);
    let app = app(&config, Router::new());

    let allowed = send(
        &app,
        "GET",
        "/healthz",
        &[(ORIGIN.as_str(), ALLOWED_ORIGIN)],
    )
    .await;
    assert_eq!(
        allowed.header(ACCESS_CONTROL_ALLOW_ORIGIN.as_str()),
        Some(ALLOWED_ORIGIN),
    );
    // Cookies stay same-origin: the cross-origin consumer is the bearer-token
    // API, so there is no credentialed cross-origin mode to get wrong.
    assert_eq!(
        allowed.header(ACCESS_CONTROL_ALLOW_CREDENTIALS.as_str()),
        None
    );

    let refused = send(
        &app,
        "GET",
        "/healthz",
        &[(ORIGIN.as_str(), "https://evil.example.com")],
    )
    .await;
    assert_eq!(refused.header(ACCESS_CONTROL_ALLOW_ORIGIN.as_str()), None);
}

#[tokio::test]
async fn a_preflight_from_a_configured_origin_is_answered() {
    let config = config(&[("CORS_ALLOWED_ORIGINS", ALLOWED_ORIGIN)]);
    let app = app(&config, Router::new());

    let answer = send(
        &app,
        "OPTIONS",
        "/healthz",
        &[
            (ORIGIN.as_str(), ALLOWED_ORIGIN),
            (ACCESS_CONTROL_REQUEST_METHOD.as_str(), "POST"),
        ],
    )
    .await;

    assert!(answer.status.is_success(), "got: {}", answer.status);
    assert_eq!(
        answer.header(ACCESS_CONTROL_ALLOW_ORIGIN.as_str()),
        Some(ALLOWED_ORIGIN),
    );
    let methods = answer
        .header(ACCESS_CONTROL_ALLOW_METHODS.as_str())
        .expect("a preflight names the methods");
    assert!(methods.contains("POST"), "got: {methods}");
    let headers = answer
        .header(ACCESS_CONTROL_ALLOW_HEADERS.as_str())
        .expect("a preflight names the headers");
    assert!(headers.contains("authorization"), "got: {headers}");
    // Even a preflight is a response, so the rest of the policy still applies.
    assert_eq!(
        answer.header(X_CONTENT_TYPE_OPTIONS.as_str()),
        Some("nosniff")
    );
}

/// The clock is paused, so tokio advances it to the timeout rather than
/// spending thirty real seconds proving the constant.
#[tokio::test(start_paused = true)]
async fn a_request_that_never_finishes_is_abandoned() {
    let config = config(&[]);
    let never = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_hours(1)).await;
            "eventually"
        }),
    );
    let app = app(&config, never);

    let answer = send(&app, "GET", "/slow", &[]).await;

    // 503, not 408: 408 says the client was too slow to send its request,
    // which is the opposite of what happened.
    assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        answer.body["message"]
            .as_str()
            .is_some_and(|message| message.contains("too long")),
        "the timeout answers in the standard error shape, got: {}",
        answer.body,
    );
    // The response a timeout produces is still a response: it carries the id
    // that finds the log line explaining it.
    assert!(answer.header(X_REQUEST_ID).is_some());
    assert_eq!(
        answer.header(X_CONTENT_TYPE_OPTIONS.as_str()),
        Some("nosniff")
    );
}

#[tokio::test]
async fn the_headers_reach_the_single_page_application_too() {
    // The composition a production deployment runs: the SPA as the router's
    // fallback, hardened around it. Merging the probes into a router that
    // already has a fallback must work, and the shell must carry the policy.
    let build = std::env::temp_dir().join(format!("anubis-server-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&build).expect("the build output must be creatable");
    std::fs::write(
        build.join("index.html"),
        "<!doctype html><title>Anubis</title>",
    )
    .expect("index.html must be writable");

    let assets = anubis::spa::Assets::new(&build).expect("the build output must open");
    let app = app(
        &config(&[]),
        Router::new().fallback_service(assets.into_service()),
    );

    let answer = send(&app, "GET", "/settings/profile", &[]).await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.header(X_CONTENT_TYPE_OPTIONS.as_str()),
        Some("nosniff")
    );
    assert!(answer.header(X_REQUEST_ID).is_some());
    // And the probes still answer from inside that composition.
    assert_eq!(
        send(&app, "GET", "/healthz", &[]).await.status,
        StatusCode::OK
    );

    let _ignored = std::fs::remove_dir_all(&build);
}

#[tokio::test]
async fn liveness_answers_without_touching_anything() {
    let config = config(&[]);
    // The pool cannot reach a database, which is the point: liveness must not
    // depend on one, or a database blip restarts every instance at once.
    let app = app(&config, Router::new());

    let answer = send(&app, "GET", "/healthz", &[]).await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body, json!({ "status": "ok" }));
}

#[tokio::test]
async fn readiness_fails_when_the_database_is_out_of_reach() {
    let config = config(&[]);
    let app = app(&config, Router::new());

    let answer = send(&app, "GET", "/readyz", &[]).await;

    assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
    let message = answer.body["message"]
        .as_str()
        .expect("the failure answers in the standard error shape");
    assert!(message.contains("not ready"), "got: {message}");
    assert!(
        !message.contains("postgres") && !message.contains("database"),
        "the probe is public, so the body names no dependency, got: {message}",
    );
}

#[tokio::test]
async fn readiness_succeeds_against_a_live_database() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping server_flow readiness test: DATABASE_URL is not set");
        return;
    };
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("the database must be reachable");
    let app = server::harden(Router::new(), pool, &config(&[]));

    let answer = send(&app, "GET", "/readyz", &[]).await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body, json!({ "status": "ready" }));
}

/// Reads one HTTP response off a socket, to the end of the connection.
async fn read_response(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the response must arrive");
    String::from_utf8_lossy(&raw).into_owned()
}

/// Sends one request and closes the write half, so the answer ends at EOF.
async fn request_over(stream: &mut TcpStream, path: &str) {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request must be written");
}

#[tokio::test]
async fn the_server_drains_in_flight_requests_and_then_exits() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port must be available");
    let address = listener
        .local_addr()
        .expect("a bound listener has an address");

    // The handler reports the moment it starts, so the shutdown signal can be
    // fired while a request is provably in flight rather than hopefully so.
    let (started, mut starts) = tokio::sync::mpsc::channel::<()>(1);
    let slow = Router::new().route(
        "/slow",
        get(move || {
            let started = started.clone();
            async move {
                let _ignored = started.send(()).await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                "finished"
            }
        }),
    );

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let app = app(&config(&[]), slow);
    let serving = tokio::spawn(server::serve_with_shutdown(listener, app, async {
        let _stopped = stopped.await;
    }));

    // The server is live: this also proves the real accept loop wires up the
    // connect info and the middleware, not just `oneshot`.
    let mut healthy = TcpStream::connect(address)
        .await
        .expect("the server must accept");
    request_over(&mut healthy, "/healthz").await;
    let response = read_response(&mut healthy).await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(
        response.to_ascii_lowercase().contains(X_REQUEST_ID),
        "got: {response}",
    );

    // A request in flight when shutdown starts still gets its answer.
    let mut draining = TcpStream::connect(address)
        .await
        .expect("the server must accept");
    request_over(&mut draining, "/slow").await;
    starts.recv().await.expect("the handler must start");
    stop.send(()).expect("the server must still be listening");

    let response = read_response(&mut draining).await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(response.ends_with("finished"), "got: {response}");

    // And the server itself is gone, well inside the drain window.
    let outcome = tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("the server must exit once its last request is answered");
    outcome
        .expect("the serving task must not panic")
        .expect("a drained shutdown is a clean exit");
}
