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
    ACCEPT_ENCODING, ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
    ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_METHOD,
    CONTENT_ENCODING, CONTENT_SECURITY_POLICY, ORIGIN, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    VARY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as ClientMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
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

/// One answer kept as the bytes that went over the wire.
///
/// The compression tests need those bytes: a compressed body is not JSON and
/// not UTF-8, and its length is the whole point.
struct RawAnswer {
    headers: HeaderMap,
    body: Vec<u8>,
}

impl RawAnswer {
    fn header(&self, name: &axum::http::HeaderName) -> Option<&str> {
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

async fn send_raw(router: &Router, path: &str, headers: &[(&str, &str)]) -> RawAnswer {
    let mut builder = Request::builder().method("GET").uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");

    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes()
        .to_vec();

    RawAnswer { headers, body }
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
        Some(
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; font-src 'self'; \
             connect-src 'self' ws://127.0.0.1:3000; base-uri 'self'; form-action 'self'; \
             frame-ancestors 'none'"
        ),
    );
}

/// The policy is what the browser is handed with the application shell, so the
/// document response is where it has to be right.
#[tokio::test]
async fn the_single_page_application_shell_carries_the_whole_policy() {
    let build = temporary_build();
    let assets = anubis::spa::Assets::new(&build).expect("the build output must open");
    let app = app(
        &config(&[("APP_URL", "https://app.example.com")]),
        Router::new().fallback_service(assets.into_service()),
    );

    let answer = send(&app, "GET", "/settings/profile", &[]).await;
    let policy = answer
        .header(CONTENT_SECURITY_POLICY.as_str())
        .expect("the shell carries a policy");

    // The shell loads one hashed module and one stylesheet, both same-origin.
    assert!(policy.contains("script-src 'self';"), "got: {policy}");
    assert!(!policy.contains("'unsafe-eval'"), "got: {policy}");
    // The component libraries write stylesheets into the document as they run.
    assert!(
        policy.contains("style-src 'self' 'unsafe-inline';"),
        "got: {policy}",
    );
    // The avatar picker previews a chosen file through a blob: URL.
    assert!(
        policy.contains("img-src 'self' data: blob:;"),
        "got: {policy}"
    );
    // The realtime channel, named rather than left to `'self'` matching.
    assert!(
        policy.contains("connect-src 'self' wss://app.example.com;"),
        "got: {policy}",
    );
    assert!(policy.contains("default-src 'none';"), "got: {policy}");
    assert!(policy.contains("base-uri 'self';"), "got: {policy}");
    assert!(policy.contains("form-action 'self';"), "got: {policy}");
    assert!(policy.contains("frame-ancestors 'none'"), "got: {policy}");

    let _ignored = std::fs::remove_dir_all(&build);
}

#[tokio::test]
async fn an_application_adds_its_own_sources_without_losing_the_frameworks() {
    let app = app(
        &config(&[
            ("APP_URL", "https://app.example.com"),
            (
                "CSP_ALLOWED_SOURCES",
                "script-src https://plausible.io; connect-src https://plausible.io",
            ),
        ]),
        Router::new(),
    );

    let answer = send(&app, "GET", "/healthz", &[]).await;
    let policy = answer
        .header(CONTENT_SECURITY_POLICY.as_str())
        .expect("every response carries a policy");

    assert!(
        policy.contains("script-src 'self' https://plausible.io;"),
        "got: {policy}",
    );
    assert!(
        policy.contains("connect-src 'self' wss://app.example.com https://plausible.io;"),
        "got: {policy}",
    );
}

/// A configuration that would widen the policy silently stops the boot instead.
#[test]
fn a_policy_a_deployment_cannot_have_is_refused_at_startup() {
    for value in [
        "default-src https://anything.example.com",
        "script-src 'unsafe-eval'",
        "script-src https:",
    ] {
        let owned = value.to_owned();
        let loaded = AppConfig::from_lookup(move |name| {
            (name == "CSP_ALLOWED_SOURCES").then(|| owned.clone())
        });

        let error = loaded.expect_err("a policy the framework will not send must stop the boot");
        assert_eq!(error.variable(), "CSP_ALLOWED_SOURCES", "for {value:?}");
    }
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

/// What an API list endpoint answers with: repetitive JSON, large enough that
/// compressing it is the difference a page load feels.
fn a_list_endpoint() -> Router {
    Router::new().route(
        "/api/v1/things",
        get(|| async {
            let things: Vec<Value> = (0..200)
                .map(|index| json!({ "id": index, "name": "a tangible thing", "state": "active" }))
                .collect();
            axum::Json(json!({ "things": things }))
        }),
    )
}

#[tokio::test]
async fn a_client_that_accepts_compression_gets_it() {
    let app = app(&config(&[]), a_list_endpoint());

    let plain = send_raw(&app, "/api/v1/things", &[]).await;
    let compressed = send_raw(&app, "/api/v1/things", &[("accept-encoding", "gzip")]).await;

    assert_eq!(compressed.header(&CONTENT_ENCODING), Some("gzip"));
    // The gzip magic number: the body really is the encoding it claims.
    assert_eq!(
        compressed.body.get(..2),
        Some([0x1f, 0x8b].as_slice()),
        "the body must start with the gzip header",
    );
    assert!(
        compressed.body.len() * 4 < plain.body.len(),
        "{} bytes compressed against {} plain is not worth the header",
        compressed.body.len(),
        plain.body.len(),
    );
    // Without it, a shared cache could hand these bytes to a client that never
    // asked for an encoding.
    assert_eq!(compressed.header(&VARY), Some("accept-encoding"));
}

#[tokio::test]
async fn brotli_is_used_when_the_client_offers_it() {
    let app = app(&config(&[]), a_list_endpoint());

    let answer = send_raw(&app, "/api/v1/things", &[("accept-encoding", "br, gzip")]).await;

    assert_eq!(answer.header(&CONTENT_ENCODING), Some("br"));
}

#[tokio::test]
async fn a_client_that_asks_for_nothing_gets_the_bytes_as_they_are() {
    let app = app(&config(&[]), a_list_endpoint());

    let answer = send_raw(&app, "/api/v1/things", &[]).await;

    assert_eq!(answer.header(&CONTENT_ENCODING), None);
    let body: Value = serde_json::from_slice(&answer.body).expect("plain bodies stay JSON");
    assert_eq!(body["things"].as_array().map(Vec::len), Some(200));
}

/// A handful of bytes costs more in headers than compressing them saves, so the
/// probes answer as themselves however the client asks.
#[tokio::test]
async fn a_tiny_body_is_not_worth_compressing() {
    let app = app(&config(&[]), Router::new());

    let answer = send_raw(&app, "/healthz", &[("accept-encoding", "br, gzip")]).await;

    assert_eq!(answer.header(&CONTENT_ENCODING), None);
    assert_eq!(
        serde_json::from_slice::<Value>(&answer.body).expect("the probe answers JSON"),
        json!({ "status": "ok" }),
    );
}

/// The realtime socket runs through the same stack the compression layer is
/// in, and a `101` carries no body to compress. Proven over a real connection,
/// because an upgrade is one of the few things `oneshot` cannot show.
#[tokio::test]
async fn a_websocket_still_upgrades_through_the_hardened_stack() {
    use axum::extract::ws::{Message as ServerMessage, WebSocket, WebSocketUpgrade};

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port must be available");
    let address = listener
        .local_addr()
        .expect("a bound listener has an address");

    let sockets = Router::new().route(
        "/realtime",
        get(|upgrade: WebSocketUpgrade| async move {
            upgrade.on_upgrade(|mut socket: WebSocket| async move {
                while let Some(Ok(message)) = socket.recv().await {
                    if let ServerMessage::Text(text) = message {
                        let _ignored = socket.send(ServerMessage::Text(text)).await;
                    }
                }
            })
        }),
    );

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let app = app(&config(&[]), sockets);
    let serving = tokio::spawn(server::serve_with_shutdown(listener, app, async {
        let _stopped = stopped.await;
    }));

    let mut request = format!("ws://{address}/realtime")
        .into_client_request()
        .expect("the websocket URL must parse");
    // The header that would tempt a compression layer to touch the upgrade.
    request.headers_mut().insert(
        ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("br, gzip"),
    );

    let (mut socket, response) = connect_async(request)
        .await
        .expect("the upgrade must succeed");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert!(
        response.headers().get(CONTENT_ENCODING).is_none(),
        "an upgrade must not be encoded",
    );

    socket
        .send(ClientMessage::text("ping"))
        .await
        .expect("the socket must accept a frame");
    let echoed = socket
        .next()
        .await
        .expect("the echo must arrive")
        .expect("the socket must stay open")
        .into_text()
        .expect("the echo is a text frame");
    assert_eq!(echoed.as_str(), "ping");

    let _closed = socket.close(None).await;
    stop.send(()).expect("the server must still be listening");
    let outcome = tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("the server must exit once the socket closes");
    outcome
        .expect("the serving task must not panic")
        .expect("a drained shutdown is a clean exit");
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

/// A directory shaped like a frontend build: a shell and one hashed asset.
///
/// The caller removes it; every test that takes one drives a real
/// `anubis::spa::Assets` over it.
fn temporary_build() -> std::path::PathBuf {
    let build = std::env::temp_dir().join(format!("anubis-server-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(build.join("assets")).expect("the build output must be creatable");
    std::fs::write(
        build.join("index.html"),
        "<!doctype html><title>Anubis</title>",
    )
    .expect("index.html must be writable");
    // A stand-in for the bundle, the largest thing a cold load pulls.
    std::fs::write(
        build.join("assets").join("index-a1b2c3.js"),
        "console.log('anubis');\n".repeat(500),
    )
    .expect("the asset must be writable");

    build
}

#[tokio::test]
async fn the_headers_reach_the_single_page_application_too() {
    // The composition a production deployment runs: the SPA as the router's
    // fallback, hardened around it. Merging the probes into a router that
    // already has a fallback must work, and the shell must carry the policy.
    let build = temporary_build();

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

    // The bundle is what a cold load waits on, so it is what compression is
    // for: the file service reads it off disk and the layer above encodes it.
    let bundle = "/assets/index-a1b2c3.js";
    let plain = send_raw(&app, bundle, &[]).await;
    let compressed = send_raw(&app, bundle, &[("accept-encoding", "gzip")]).await;

    assert_eq!(compressed.header(&CONTENT_ENCODING), Some("gzip"));
    assert!(
        compressed.body.len() * 10 < plain.body.len(),
        "{} bytes compressed against {} plain",
        compressed.body.len(),
        plain.body.len(),
    );
    // Compression must not cost the asset its year-long cache entry.
    assert_eq!(
        compressed.header(&axum::http::header::CACHE_CONTROL),
        Some("public, max-age=31536000, immutable"),
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
