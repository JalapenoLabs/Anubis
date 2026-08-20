//! The production serve path every Anubis application boots through.
//!
//! [`serve`] is the whole story: it mounts the liveness and readiness probes,
//! wraps the application's router in the framework's middleware, binds the
//! configured address, and serves until the process is asked to stop. An
//! application's `main` composes routers and calls it once.
//!
//! ```no_run
//! # async fn boot(
//! #     app: axum::Router,
//! #     pool: anubis::db::DbPool,
//! #     config: &anubis::config::AppConfig,
//! # ) {
//! anubis::server::serve(app, pool, config)
//!     .await
//!     .expect("the server terminated unexpectedly");
//! # }
//! ```
//!
//! # The middleware stack
//!
//! [`harden`] applies these, listed outermost first. The order is a decision,
//! not an accident: every layer below the request id is logged with it, and
//! every layer above the timeout still stamps its headers on the response a
//! timeout produces.
//!
//! 1. **Request id.** Each request is assigned a fresh UUID, carried in the
//!    tracing span, returned as `x-request-id`, and available to handlers as a
//!    [`RequestId`] extension. An inbound `x-request-id` is overwritten rather
//!    than honored: the id is the server's own correlation handle, and a caller
//!    that could choose it could make two unrelated requests share one.
//! 2. **Tracing.** One event per completed request, inside a span carrying the
//!    method, the path, and the id, at `INFO` (`WARN` for a `5xx`).
//! 3. **Security headers.** See [`headers`] for the policy and its reasoning.
//! 4. **CORS**, only when `CORS_ALLOWED_ORIGINS` names origins. Unset means no
//!    CORS headers at all, which is the strictest posture a browser honors.
//! 5. **Compression.** Responses a client said it accepts compressed are
//!    compressed, brotli or gzip; see below.
//! 6. **Request timeout**, innermost, so the response it produces still
//!    receives an id and the security headers on its way out.
//!
//! # Compression
//!
//! One layer covers everything the server sends: the JavaScript bundle and the
//! stylesheet a cold page load pulls, `index.html`, and every JSON body the API
//! answers with. Text compresses by roughly three to four times, and on the
//! first load of an application that is most of what the browser waits for.
//!
//! The layer sits below CORS and the security headers, so a compressed response
//! leaves with the same headers and the same request id as any other, and above
//! the timeout, so it sees every route including the single-page-application
//! fallback. Compression is negotiated, never assumed: a client that sends no
//! `accept-encoding` gets the bytes as they were, and every compressed response
//! carries `vary: accept-encoding` so a shared cache keeps the variants apart.
//!
//! What is left alone is as deliberate as what is not. Responses under 32
//! bytes, where a compression header costs more than the body saves; images and
//! anything else already compressed; server-sent event streams, which must
//! flush per event; and websocket upgrades, whose `101` carries no body at all.
//! A response that arrives already encoded, a precompressed file straight off
//! disk among them, passes through untouched.
//!
//! # Shutdown
//!
//! `SIGTERM` (the signal an orchestrator sends first) and ctrl-c both start a
//! graceful shutdown: the listener stops accepting immediately and in-flight
//! requests get [`SHUTDOWN_DRAIN`] to finish. Windows has no `SIGTERM`, so
//! ctrl-c is the whole story there.
//!
//! Applications that must coordinate shutdown with something else, a
//! [`crate::jobs::Worker`] above all, drive both from one signal with
//! [`serve_with_shutdown`].
//!
//! # Probes
//!
//! `GET /healthz` answers liveness: it touches nothing, so it fails only when
//! the process itself is gone. `GET /readyz` answers readiness: it checks out a
//! database connection under a short timeout and answers `503` when it cannot.
//! Point an orchestrator's restart policy at `/healthz` and its traffic gate at
//! `/readyz`, so a database blip drains traffic instead of restarting pods.

mod headers;
mod health;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::compression::CompressionLayer;
use tower_http::trace::{MakeSpan, OnResponse, TraceLayer};
use tracing::Span;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::db::DbPool;
use crate::http::ApiError;

#[doc(inline)]
pub use headers::{CorsConfig, CspConfig};

/// The directive vocabulary `CSP_ALLOWED_SOURCES` is validated against.
///
/// Configuration is the only caller; applications name directives in that
/// variable rather than in code.
pub(crate) use headers::extendable_directive;

/// The policy the API reference page sends instead of the application's.
///
/// The one response in the framework that overrides the policy; see
/// [`headers`] for why, and what it widens.
pub(crate) use headers::API_REFERENCE_CSP;

/// The probe routes, for the manifest test that keeps the route table honest.
///
/// Applications get them from [`harden`], so nothing outside tests needs this.
#[cfg(test)]
pub(crate) use health::router as health_router;

/// How long one request may run before the server gives up on it.
///
/// Generous on purpose: an avatar upload decodes and re-encodes an image, and
/// the file handling that follows it will be slower still. The value is a
/// backstop against a request that will never finish, not a latency budget, so
/// it sits far above anything a healthy handler takes. Lowering it below the
/// slowest legitimate handler turns a slow success into a failure.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long in-flight requests get to finish once shutdown starts.
///
/// Sized to fit inside Kubernetes' default 30-second `terminationGracePeriod`,
/// so the process exits on its own terms rather than being killed mid-response.
/// A request that started just before the signal and wants the full
/// [`REQUEST_TIMEOUT`] is therefore cut short, which is deliberate: by then the
/// traffic gate has already stopped sending work, and a deploy that waits on
/// one straggler is a deploy that hangs.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(25);

/// The header carrying a request's id back to the caller.
const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The id one request carries through the logs and back to its caller.
///
/// Every response carries it as `x-request-id`. Error bodies stay generic, so
/// this is the handle that ties a user's report to the log line that explains
/// it. Handlers read it with `Extension<RequestId>` when they want to name it
/// in an event of their own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Mints a fresh id.
    fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Returns the id as it appears in the header and in the logs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RequestId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Binds the configured address and serves until the process is asked to stop.
///
/// Applies everything [`harden`] does and drains on `SIGTERM` or ctrl-c; see
/// the module docs.
///
/// # Errors
/// Returns an [`Error`] when the address cannot be bound, or when the accept
/// loop fails.
pub async fn serve(router: Router, pool: DbPool, config: &AppConfig) -> Result<(), Error> {
    let app = harden(router, pool, config);

    let configured = config.server.socket_addr();
    let listener = TcpListener::bind(configured)
        .await
        .map_err(|source| Error::new("failed to bind the server address", source))?;

    // The bound address, which differs from the configured one only when the
    // port was 0 and the kernel picked it.
    let address = listener.local_addr().unwrap_or(configured);
    tracing::info!(
        server.address = %address,
        app.environment = %config.environment,
        framework.version = crate::VERSION,
        "server listening on {{server.address}}",
    );

    serve_with_shutdown(listener, app, shutdown_signal()).await
}

/// Serves a prepared application on `listener` until `shutdown` resolves.
///
/// The escape hatch behind [`serve`], for applications that shut something
/// else down alongside the server, a [`crate::jobs::Worker`] above all. Pass
/// the app through [`harden`] first; nothing here adds middleware.
///
/// Once `shutdown` resolves the listener closes at once and in-flight requests
/// get [`SHUTDOWN_DRAIN`] to finish, after which the server returns anyway.
///
/// ```no_run
/// # async fn boot(app: axum::Router, listener: tokio::net::TcpListener) {
/// let (stop, stopped) = tokio::sync::watch::channel(());
/// let mut worker_signal = stopped.clone();
///
/// anubis::server::serve_with_shutdown(listener, app, async move {
///     let _stopped = worker_signal.changed().await;
/// })
/// .await
/// .expect("the server terminated unexpectedly");
/// # let _ = stop;
/// # }
/// ```
///
/// # Errors
/// Returns an [`Error`] when the accept loop fails.
pub async fn serve_with_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Error> {
    let (drain_started, drain_starts) = oneshot::channel();

    // Connect info, not just the router: the framework's rate limits charge
    // each request to the address it arrived from, which only the accept loop
    // knows. See `crate::rate_limit`.
    let serving = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.await;
        // The receiver is gone only if the server already stopped, in which
        // case there is nothing left to drain.
        let _ignored = drain_started.send(());
    })
    .into_future();
    let mut serving = std::pin::pin!(serving);

    tokio::select! {
        finished = &mut serving => return finished.map_err(stopped),
        _started = drain_starts => {}
    }

    tracing::info!(
        server.drain_ms = SHUTDOWN_DRAIN.as_millis(),
        "shutting down: the listener is closed and in-flight requests have {{server.drain_ms}}ms",
    );

    match tokio::time::timeout(SHUTDOWN_DRAIN, serving).await {
        Ok(finished) => finished.map_err(stopped),
        Err(_elapsed) => {
            tracing::warn!(
                server.drain_ms = SHUTDOWN_DRAIN.as_millis(),
                "the {{server.drain_ms}}ms drain window expired with requests still in flight; \
                 exiting anyway",
            );
            Ok(())
        }
    }
}

/// Wraps an application's router in the framework's production middleware.
///
/// Mounts the probes described in the module docs and applies the middleware
/// stack listed there. [`serve`] calls this; call it directly only to drive the
/// same application from somewhere else, such as an integration test.
pub fn harden(router: Router, pool: DbPool, config: &AppConfig) -> Router {
    // Each call wraps what came before, so the last layer is the outermost.
    // The stack is documented outermost-first in the module docs; read this
    // bottom to top to match it.
    let app = router
        .merge(health::router(pool))
        .layer(axum::middleware::from_fn(enforce_timeout))
        // Above the timeout so it covers every route, including the SPA
        // fallback, and below the header layers so a compressed response is
        // stamped like any other. See the module docs for what it skips.
        .layer(CompressionLayer::new());
    let app = headers::allow_cross_origin(app, config);
    let app = headers::secure(app, config);

    app.layer(
        TraceLayer::new_for_http()
            .make_span_with(RequestSpan)
            // The span already names the request; one event per completed
            // request keeps a busy log readable.
            .on_request(())
            .on_response(LogResponse)
            // A 5xx is already reported by `LogResponse` at WARN, and the
            // handler that produced it logged the cause.
            .on_failure(()),
    )
    .layer(axum::middleware::from_fn(assign_request_id))
}

/// Assigns each request an id, and returns it in the response.
async fn assign_request_id(mut request: Request, next: Next) -> Response<axum::body::Body> {
    let id = RequestId::new();
    let header =
        HeaderValue::from_str(id.as_str()).expect("a UUID renders as a valid header value");

    // The request carries the same id the response will, so an inner layer
    // reading the header and a handler reading the extension always agree.
    request.headers_mut().insert(X_REQUEST_ID, header.clone());
    request.extensions_mut().insert(id);

    let mut response = next.run(request).await;
    response.headers_mut().insert(X_REQUEST_ID, header);
    response
}

/// Abandons a request that has run longer than [`REQUEST_TIMEOUT`].
///
/// Dropping the handler's future is what a timeout means in async Rust: the
/// work stops at its next await point, and any open transaction rolls back
/// when its connection returns to the pool.
async fn enforce_timeout(request: Request, next: Next) -> Response<axum::body::Body> {
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!(
                http.request.timeout_ms = REQUEST_TIMEOUT.as_millis(),
                "the request exceeded the {{http.request.timeout_ms}}ms timeout and was abandoned",
            );
            // 503 rather than 408: 408 says the *client* was too slow to send
            // its request, which is the opposite of what happened. 503 says
            // this server could not answer in time and the caller may retry,
            // which is exactly the situation.
            ApiError::unavailable("The server took too long to answer this request.")
                .into_response()
        }
    }
}

/// Names each request in the log, so every event under it carries its id.
#[derive(Debug, Clone, Copy)]
struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> Span {
        let id = request
            .extensions()
            .get::<RequestId>()
            .map_or("-", RequestId::as_str);

        tracing::info_span!(
            "http.request",
            http.request.method = %request.method(),
            url.path = %request.uri().path(),
            request.id = id,
        )
    }
}

/// Logs one event per completed request, inside its span.
#[derive(Debug, Clone, Copy)]
struct LogResponse;

impl<B> OnResponse<B> for LogResponse {
    fn on_response(self, response: &Response<B>, latency: Duration, _span: &Span) {
        let status = response.status().as_u16();
        let duration = latency.as_millis();

        if response.status().is_server_error() {
            tracing::warn!(
                http.response.status_code = status,
                http.server.request.duration_ms = duration,
                "request failed with {{http.response.status_code}} in \
                 {{http.server.request.duration_ms}}ms",
            );
        } else {
            tracing::info!(
                http.response.status_code = status,
                http.server.request.duration_ms = duration,
                "request answered {{http.response.status_code}} in \
                 {{http.server.request.duration_ms}}ms",
            );
        }
    }
}

/// Resolves when the process is asked to stop.
///
/// `SIGTERM` is what an orchestrator sends before it kills a container, and
/// ctrl-c is what a developer sends. Windows has neither `SIGTERM` nor the
/// rest of the POSIX signal set, so ctrl-c is the whole story there.
async fn shutdown_signal() {
    let interrupt = async {
        if let Err(source) = tokio::signal::ctrl_c().await {
            tracing::error!(
                error.message = %source,
                "could not listen for ctrl-c, so it will not drain: {{error.message}}",
            );
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(source) => {
                tracing::error!(
                    error.message = %source,
                    "could not listen for SIGTERM, so a deploy will not drain: {{error.message}}",
                );
                std::future::pending::<()>().await;
            }
        }
    };

    // Windows has no SIGTERM; tokio maps the console close events onto ctrl-c.
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => tracing::info!(
            signal.name = "ctrl-c",
            "received {{signal.name}}: draining",
        ),
        () = terminate => tracing::info!(
            signal.name = "SIGTERM",
            "received {{signal.name}}: draining",
        ),
    }
}

/// Renders an accept-loop failure as this module's error.
fn stopped(source: std::io::Error) -> Error {
    Error::new("the server stopped", source)
}

/// The server could not bind its address, or its accept loop failed.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: std::io::Error,
    backtrace: Backtrace,
}

impl Error {
    fn new(context: &'static str, source: std::io::Error) -> Self {
        Self {
            context,
            source,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::{REQUEST_TIMEOUT, RequestId, SHUTDOWN_DRAIN};

    #[test]
    fn ids_are_unique_and_render_as_they_are_logged() {
        let first = RequestId::new();
        let second = RequestId::new();

        assert_ne!(first, second);
        assert_eq!(first.to_string(), first.as_str());
        // A UUID, which is what makes it safe to put in a header verbatim.
        assert_eq!(first.as_str().len(), 36);
    }

    #[test]
    fn the_drain_window_fits_inside_a_default_grace_period() {
        // The whole point of the constant: a bounded drain that finishes
        // before an orchestrator loses patience and kills the process.
        assert!(SHUTDOWN_DRAIN < std::time::Duration::from_secs(30));
        assert!(SHUTDOWN_DRAIN <= REQUEST_TIMEOUT);
    }
}
