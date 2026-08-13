//! Liveness and readiness probes.
//!
//! Two endpoints answering two different questions, because an orchestrator
//! acts on the answers differently.
//!
//! `GET /healthz` is liveness: is this process still a process? It touches
//! nothing outside itself, so it can only fail when the answer is genuinely no.
//! A restart policy points here. Making it check the database would turn a
//! database blip into a restart loop across every instance at once.
//!
//! `GET /readyz` is readiness: can this instance serve traffic right now? It
//! checks out a pooled database connection under [`READINESS_TIMEOUT`] and
//! answers `503` when it cannot. A load balancer's traffic gate points here, so
//! an instance that cannot reach the database is taken out of rotation and put
//! back when it recovers, with no restart involved.
//!
//! Both are mounted for every application by [`crate::server::harden`].

use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::db::DbPool;
use crate::http::ApiError;

/// How long readiness waits for a pooled connection before giving up.
///
/// Short on purpose: a traffic gate polls this every few seconds, and an
/// instance that cannot produce a connection in two seconds is already failing
/// its users. Waiting longer only delays the moment traffic moves elsewhere.
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

/// The one-field body both probes answer with.
#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
}

/// Mounts `/healthz` and `/readyz`.
pub(crate) fn router(pool: DbPool) -> Router {
    Router::new()
        .route("/healthz", get(alive))
        .route("/readyz", get(ready))
        .with_state(pool)
}

/// Answers liveness, in process, with no I/O of any kind.
async fn alive() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Answers readiness by proving the database pool can still hand out a
/// connection.
///
/// Checking one out is the whole check: the pool validates a recycled
/// connection before returning it, so a connection in hand means the database
/// answered.
async fn ready(State(pool): State<DbPool>) -> Response {
    match tokio::time::timeout(READINESS_TIMEOUT, pool.get()).await {
        Ok(Ok(connection)) => {
            drop(connection);
            Json(Health { status: "ready" }).into_response()
        }
        Ok(Err(source)) => {
            tracing::warn!(
                error.message = %source,
                "not ready: the database pool has no connection to give ({{error.message}})",
            );
            not_ready()
        }
        Err(_elapsed) => {
            tracing::warn!(
                db.timeout_ms = READINESS_TIMEOUT.as_millis(),
                "not ready: the database pool did not answer within {{db.timeout_ms}}ms",
            );
            not_ready()
        }
    }
}

/// The `503` a failed readiness check answers with.
///
/// The body says nothing about which dependency failed. The probe is
/// unauthenticated and reachable by anyone; the log line beside it names the
/// cause for the operator who needs it.
fn not_ready() -> Response {
    ApiError::unavailable("The service is not ready.").into_response()
}
