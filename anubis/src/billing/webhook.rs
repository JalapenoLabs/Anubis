//! The endpoint Stripe posts subscription events to.
//!
//! | Method | Path | Authentication |
//! |---|---|---|
//! | POST | `/webhooks/stripe-billing` | a signature, not a session |
//!
//! # Framework-mounted, and out of the application's way
//!
//! Billing is a framework feature, so its receiver is the framework's own:
//! [`router`] is mounted by the application under `/webhooks` beside whatever
//! receivers `anubis scaffold webhook` generated. The path is
//! `stripe-billing` rather than `stripe` precisely so an application that
//! scaffolds its own Stripe receiver, for Connect accounts or for payments this
//! framework knows nothing about, keeps `/webhooks/stripe` for itself. The two
//! endpoints are separate subscriptions in the Stripe dashboard, with separate
//! signing secrets and separate event lists.
//!
//! # Store first, ask questions afterwards
//!
//! The handler writes the event down, queues a job, and answers `200`. Both
//! writes happen in one transaction, so a stored event always has work queued
//! for it and a queued job always has a row to read. Everything the event
//! *means* is decided later, in [`super::lifecycle`], where a failure retries
//! instead of being lost. This is the discipline `docs/webhooks.md` describes
//! for generated receivers, and the reasoning is the same: a provider retries a
//! slow receiver and eventually disables it, so the endpoint should be as fast
//! as one insert and as durable as the database.
//!
//! # Why an unverified request is refused rather than stored
//!
//! A generated receiver stores what it cannot verify, because its
//! `verify_signature` is a function the developer has to finish and a check
//! that is subtly wrong must not look like silence. The framework has no such
//! excuse here: Stripe's scheme is known, implemented, and tested
//! ([`crate::webhooks::signature::verify_stripe`]), so a request that does not
//! verify is either an attacker or a mistyped secret, and neither is worth a
//! row. Stripe's own guidance is to answer `400`, which is what this does, and
//! the response message says why, so the failure is legible in Stripe's
//! delivery log, where an operator is already looking.
//!
//! Storing refused requests was considered and rejected: the endpoint is
//! public and unauthenticated, so a table anyone on the internet can write to
//! is a liability rather than evidence.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use diesel_async::AsyncConnection;
use serde_json::{Value as JsonValue, json};

use super::event::{NewStripeBillingEvent, StripeBillingEvent};
use super::lifecycle::ProcessStripeEvent;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::webhooks::signature;

/// Where Stripe posts billing events, as an application mounts it.
///
/// This is the URL to register in the Stripe dashboard, under `APP_URL`. The
/// path is public knowledge: what makes a request trustworthy is the signature,
/// never the address.
pub const WEBHOOK_PATH: &str = "/webhooks/stripe-billing";

/// The route inside this router, which the application nests under `/webhooks`.
const ROUTE: &str = "/stripe-billing";

/// The environment variable holding the secret Stripe signs with.
///
/// Named here as well as in [`crate::config`] so the `503` this endpoint
/// answers without it can say exactly what to set.
pub const WEBHOOK_SECRET_VAR: &str = "STRIPE_WEBHOOK_SECRET";

/// Returns the Stripe billing receiver, to mount under `/webhooks`.
///
/// The router carries no session layer and no rate limit, for the reasons every
/// receiver does not: the caller is a machine with a signature rather than a
/// person with a cookie, and webhooks arrive in bursts that a limiter sized for
/// humans would refuse. What bounds the work is its shape, one insert over a
/// body the server already caps.
///
/// ```ignore
/// let app = Router::new()
///     .nest("/webhooks", anubis::billing::webhook_router(pool.clone(), &config))
///     .nest("/webhooks", my_app::webhooks_router(&pool));
/// ```
pub fn router(pool: DbPool, config: &AppConfig) -> Router {
    let secret = config
        .stripe
        .as_ref()
        .and_then(|stripe| stripe.webhook_secret())
        .map(Arc::from);

    Router::new()
        .route(ROUTE, post(receive))
        .with_state(WebhookState { pool, secret })
}

#[derive(Clone)]
struct WebhookState {
    pool: DbPool,
    /// Read once at boot: the value cannot change without a restart anyway.
    secret: Option<Arc<str>>,
}

/// Stores one verified event and queues the job that will act on it.
async fn receive(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let Some(secret) = state.secret.as_deref() else {
        // Nothing can be trusted without the secret, and acting on an
        // unverified event would let anyone with the URL grant themselves a
        // paid plan. Answering `503` rather than `200` also puts the problem in
        // Stripe's delivery log instead of silently dropping real events.
        tracing::error!(
            billing.secret_var = WEBHOOK_SECRET_VAR,
            "a Stripe billing event arrived but {{billing.secret_var}} is unset, so it \
             could not be verified and was refused",
        );
        return Err(ApiError::unavailable(format!(
            "Billing webhooks are not configured for this deployment. Set \
             {WEBHOOK_SECRET_VAR} to enable them.",
        )));
    };

    // Verified against the bytes that arrived, before anything parses them: any
    // JSON library is free to reorder keys on a round trip, and the signature
    // covers these exact bytes.
    let presented = headers
        .get(signature::STRIPE_SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !signature::verify_stripe(secret, presented, &body, Utc::now().timestamp()) {
        tracing::warn!(
            http.request.body.size = body.len(),
            billing.signature.present = !presented.is_empty(),
            "a Stripe billing event failed signature verification and was refused",
        );
        return Err(ApiError::validation(
            "The Stripe-Signature header did not verify against STRIPE_WEBHOOK_SECRET.",
        ));
    }

    let payload = serde_json::from_slice::<JsonValue>(&body)
        .map_err(|_source| ApiError::validation("Expected a JSON body."))?;
    let Some(envelope) = Envelope::read(&payload) else {
        return Err(ApiError::validation(
            "Expected a Stripe event carrying an id and a type.",
        ));
    };

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let stored = connection
        .transaction::<_, ApiError, _>(async |connection| {
            let Some(record) = StripeBillingEvent::store(
                connection,
                NewStripeBillingEvent {
                    stripe_event_id: envelope.id,
                    event_type: envelope.event_type,
                    payload: &payload,
                    stripe_created_at: envelope.created,
                },
            )
            .await?
            else {
                return Ok(None);
            };

            // In the same transaction as the insert, so the row and its work
            // commit together: a rollback queues nothing, and a stored event is
            // never left with nobody coming for it.
            crate::jobs::enqueue(
                connection,
                &ProcessStripeEvent {
                    event_id: record.id,
                },
            )
            .await
            .map_err(log_internal)?;

            Ok(Some(record.id))
        })
        .await?;

    let id = if let Some(id) = stored {
        tracing::info!(
            billing.event.id = envelope.id,
            billing.event.type = envelope.event_type,
            "stored Stripe billing event {{billing.event.id}} ({{billing.event.type}})",
        );
        id
    } else {
        // Stripe redelivers an event it did not hear a `2xx` for, so this is
        // routine rather than suspicious. The first delivery already has a job
        // coming for it.
        tracing::info!(
            billing.event.id = envelope.id,
            billing.event.type = envelope.event_type,
            "Stripe redelivered event {{billing.event.id}}, which is already stored",
        );
        StripeBillingEvent::find_by_stripe_id(&mut connection, envelope.id)
            .await
            .map_err(log_internal)?
            .map(|record| record.id)
            .ok_or_else(|| {
                tracing::error!(
                    billing.event.id = envelope.id,
                    "event {{billing.event.id}} conflicted on insert and then vanished",
                );
                ApiError::internal()
            })?
    };

    // `200` the moment the row is committed. The id goes back so Stripe's own
    // delivery log and this table can be lined up when they disagree.
    Ok((StatusCode::OK, Json(json!({ "id": id }))))
}

/// The three fields of Stripe's event envelope the receiver reads.
struct Envelope<'a> {
    id: &'a str,
    event_type: &'a str,
    /// When Stripe created the event, which is how ordering is decided later.
    created: Option<DateTime<Utc>>,
}

impl<'a> Envelope<'a> {
    /// Reads the envelope, or `None` for a document that is not an event.
    fn read(payload: &'a JsonValue) -> Option<Self> {
        Some(Self {
            id: payload.get("id")?.as_str()?,
            event_type: payload.get("type")?.as_str()?,
            created: payload
                .get("created")
                .and_then(JsonValue::as_i64)
                .and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
        })
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "a Stripe billing event could not be stored: {{error.message}}",
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::{ROUTE, WEBHOOK_PATH};

    #[test]
    fn the_documented_path_is_where_the_route_actually_mounts() {
        // The application nests this router under `/webhooks`, so the constant
        // an operator pastes into Stripe and the route served have to agree.
        assert_eq!(WEBHOOK_PATH, format!("/webhooks{ROUTE}"));
        assert!(
            !WEBHOOK_PATH.starts_with("/webhooks/stripe/") && WEBHOOK_PATH != "/webhooks/stripe",
            "`/webhooks/stripe` belongs to an application scaffolding its own receiver",
        );
    }
}
