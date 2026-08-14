//! The endpoint Hypothetical Sender posts to, and the signature check on it.
//!
//! | Method | Path | Authentication |
//! |---|---|---|
//! | POST | `/webhooks/hypothetical-sender` | a signature, not a session |
//!
//! # Store first, ask questions afterwards
//!
//! The handler does exactly two things: it writes the request down, and it
//! queues a job to deal with it. Both happen in one transaction, so a stored
//! webhook always has work queued for it and a queued job always has a row to
//! read. Everything a provider's payload means is decided later, in
//! [`super::job`], where a failure retries instead of being lost.
//!
//! That ordering is not a style preference. A provider retries a slow or
//! failing receiver, often aggressively, and gives up eventually; work done
//! inside the request is work that can time out and be sent again. Answering
//! `200` as soon as the row is committed makes this endpoint as fast as one
//! insert and as durable as the database.
//!
//! # Why an unverified request is stored rather than refused
//!
//! Verification says whether the request really came from Hypothetical Sender.
//! It does **not** decide whether the row is written: an endpoint that refuses
//! at the edge throws away the one piece of evidence that explains what
//! happened, and a signature check that is subtly wrong then looks exactly like
//! silence. So the answer is recorded in `verified`, the job decides what an
//! unverified event is worth, and a developer can see both.

use std::sync::Arc;

use anubis::db::DbPool;
use anubis::http::ApiError;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use diesel_async::AsyncConnection;
use serde_json::{Value as JsonValue, json};

use super::job::ProcessHypotheticalSenderWebhook;
use super::model::HypotheticalSenderWebhook;

/// The environment variable holding the secret Hypothetical Sender signs with.
///
/// A shared secret is configuration, not application data: it differs per
/// deployment and belongs nowhere near the repository. With it unset the
/// endpoint still stores what arrives, marked unverified, and says so in the
/// log rather than pretending the check passed.
pub const SIGNING_SECRET_VAR: &str = "HYPOTHETICAL_SENDER_WEBHOOK_SECRET";

/// The header Hypothetical Sender puts its signature in.
///
/// The default is the generic spelling. Change it to whatever the provider's
/// documentation says, alongside [`verify_signature`] below.
pub const SIGNATURE_HEADER: &str = "x-webhook-signature";

/// Headers that are credentials rather than provenance, and are never stored.
///
/// Everything else is kept, because the value of a stored request is being able
/// to answer "what exactly did they send us" a week later.
const REDACTED_HEADERS: [&str; 2] = ["authorization", "cookie"];

/// Returns the Hypothetical Sender webhook route, mounted under `/webhooks`.
///
/// The router carries no authentication layer, because there is nobody to
/// authenticate: the caller is a machine with a signature. It carries no rate
/// limit either. Webhooks arrive at machine rates and in bursts, so the budgets
/// that protect sign-in would drop legitimate provider traffic, and dropping it
/// is what makes a provider retry and eventually disable the endpoint. The
/// protection here is that the work is one insert, bounded by the body limit.
pub fn router(pool: DbPool) -> Router {
    let secret = std::env::var(SIGNING_SECRET_VAR).ok();
    if secret.is_none() {
        tracing::warn!(
            webhook.provider = "Hypothetical Sender",
            webhook.secret_var = SIGNING_SECRET_VAR,
            "{{webhook.secret_var}} is unset, so {{webhook.provider}} webhooks \
             will be stored unverified",
        );
    }

    Router::new()
        .route("/hypothetical-sender", post(receive))
        .with_state(HypotheticalSenderWebhookState {
            pool,
            secret: secret.map(Arc::from),
        })
}

#[derive(Clone)]
struct HypotheticalSenderWebhookState {
    pool: DbPool,
    /// Read once at boot: the value cannot change without a restart anyway.
    secret: Option<Arc<str>>,
}

/// Returns `true` when `body` really came from Hypothetical Sender.
///
/// **This is the one function you have to finish.** What ships is the shape
/// every HMAC scheme has: read the signature the provider sent, recompute the
/// MAC over the bytes that arrived, and compare the two in constant time.
/// [`anubis::webhooks::signature::verify_hmac_sha256`] is that comparison, and
/// it is all the arithmetic any of these schemes need. What differs per
/// provider is the header's spelling and what goes into the MAC:
///
/// - **The default here**: HMAC-SHA256 over the exact request body, hex, in
///   [`SIGNATURE_HEADER`]. This is what most publishers document, and what
///   Hypothetical Sender is assumed to do until you check.
/// - **GitHub**: `X-Hub-Signature-256: sha256=<hex>`, over the exact body.
///   Strip the `sha256=` prefix and the default below is already correct.
/// - **Stripe**: `Stripe-Signature: t=<unix>,v1=<hex>`, and the MAC covers
///   `<t>.<body>` rather than the body alone. Split the header on commas, take
///   `t` and `v1`, refuse a `t` far from now, and MAC the joined message.
/// - **Another Anubis application**: it signs the way `docs/webhooks.md`
///   describes, so call [`anubis::webhooks::signature::verify`] instead and get
///   the timestamp window and the scheme list for free.
///
/// Whatever the scheme, MAC the raw bytes that arrived. Re-serializing the
/// parsed JSON is free to reorder keys and change spacing, and the signature
/// then never matches.
///
/// # Examples
/// ```ignore
/// let signed = verify_signature("whsec_example", &headers, body.as_ref());
/// ```
#[must_use]
pub fn verify_signature(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(presented) = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!(
            webhook.header = SIGNATURE_HEADER,
            "a Hypothetical Sender webhook arrived with no {{webhook.header}} header",
        );
        return false;
    };

    anubis::webhooks::signature::verify_hmac_sha256(secret, body, presented.trim())
}

/// Stores one received webhook and queues the job that will process it.
async fn receive(
    State(state): State<HypotheticalSenderWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    // Verified against the bytes that arrived, before anything parses them.
    let verified = state
        .secret
        .as_deref()
        .is_some_and(|secret| verify_signature(secret, &headers, &body));

    let Ok(payload) = serde_json::from_slice::<JsonValue>(&body) else {
        // The one request that is refused rather than stored: a body the column
        // cannot hold. Every provider sends JSON, so this is a misconfigured
        // endpoint rather than an event, and saying so is more useful than a
        // row nothing can read.
        tracing::warn!(
            http.request.body.size = body.len(),
            "a Hypothetical Sender webhook arrived with a body that is not JSON",
        );
        return Err(ApiError::validation("Expected a JSON body."));
    };
    let captured = captured_headers(&headers);

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let id = connection
        .transaction::<_, ApiError, _>(async |connection| {
            let record =
                HypotheticalSenderWebhook::store(connection, &payload, &captured, verified).await?;
            // In the same transaction as the insert, so the row and its work
            // commit together: a rollback queues nothing, and a stored webhook
            // is never left with nobody coming for it.
            anubis::jobs::enqueue(
                connection,
                &ProcessHypotheticalSenderWebhook {
                    webhook_id: record.id,
                },
            )
            .await
            .map_err(log_internal)?;
            Ok(record.id)
        })
        .await?;

    tracing::info!(
        webhook.id = %id,
        webhook.verified = verified,
        "stored a Hypothetical Sender webhook: {{webhook.id}}",
    );

    // `200` the moment the row is committed. Every provider treats a 2xx as
    // delivered, and the fastest honest 2xx is the one that keeps this endpoint
    // off a provider's retry schedule. The id goes back so that a provider's
    // own delivery log and this table can be lined up when they disagree.
    Ok((StatusCode::OK, Json(json!({ "id": id }))))
}

/// The headers worth keeping, as a JSON object.
///
/// A header sent twice keeps its last value: repeats are vanishingly rare on
/// webhook requests, and an object keyed by name is what a developer reading
/// the row wants.
fn captured_headers(headers: &HeaderMap) -> JsonValue {
    let mut captured = serde_json::Map::new();
    for (name, value) in headers {
        if REDACTED_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if let Ok(rendered) = value.to_str() {
            captured.insert(name.as_str().to_owned(), JsonValue::from(rendered));
        }
    }
    JsonValue::Object(captured)
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "hypothetical sender webhook request failed: {{error.message}}",
    );
    ApiError::internal()
}
