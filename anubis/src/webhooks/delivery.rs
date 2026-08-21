//! Deliveries: one event's journey to one endpoint, and the job that drives it.
//!
//! [`emit`](super::emit) writes a `pending` row per subscribed endpoint and
//! enqueues one [`DeliverWebhook`] job per row, both through the caller's
//! connection. A [`Deliverer`] registered on a worker then POSTs the payload,
//! signs it, and records what came back on the row.
//!
//! The row is the team's whole debugging story, which is why it outlives the
//! job: `dead_jobs` is for the operator running the application, and
//! `webhook_deliveries` is for the team whose endpoint would not answer.

use std::fmt::{self, Debug, Display, Formatter};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use super::endpoint;
use super::signature;
use crate::auth::secret_box::SecretKey;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::jobs::{BoxError, Job};
use crate::schema::{webhook_deliveries, webhook_endpoints};

/// How long one delivery attempt may take before it counts as a failure.
///
/// A receiver is expected to acknowledge quickly and do its work afterwards,
/// which is the contract every webhook publisher states. Waiting longer only
/// holds a worker slot open for an endpoint that is already misbehaving.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one "endpoint stopped answering" notice speaks for.
///
/// A receiver that is down fails every event sent to it, so the notice is
/// rate limited to one per endpoint per day: long enough that a busy team
/// hears about an outage once, short enough that an outage lasting a week is
/// mentioned more than once.
const DEAD_NOTICE_WINDOW_HOURS: i64 = 24;

/// Longest failure message kept on a delivery row.
///
/// Long enough to hold a connection error or the start of an error page, short
/// enough that an endpoint answering with a large body cannot bloat the table.
const MAX_ERROR_LENGTH: usize = 2_000;

/// Where a delivery stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Queued, and not yet attempted.
    Pending,
    /// The endpoint answered `2xx`.
    Delivered,
    /// The last attempt failed and another one is coming.
    Failed,
    /// Out of attempts. Nothing will retry this without a person.
    Dead,
}

impl DeliveryStatus {
    /// The value stored in the `status` column and served over the API.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Dead => "dead",
        }
    }
}

impl Display for DeliveryStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One attempt history: what was sent where, and what came back.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = webhook_deliveries)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WebhookDelivery {
    /// Primary key, and the value of the `anubis-webhook-id` header.
    pub id: Uuid,
    /// The endpoint this delivery is addressed to.
    pub webhook_endpoint_id: Uuid,
    /// The event type, e.g. `project.created`.
    pub event_type: String,
    /// The serialized record, exactly as the REST API answers with it.
    pub payload: JsonValue,
    /// One of [`DeliveryStatus`]'s values.
    pub status: String,
    /// Attempts spent so far.
    pub attempts: i32,
    /// The HTTP status last answered, when the endpoint answered at all.
    pub response_status: Option<i32>,
    /// The most recent failure, for the team's debugging screen.
    pub last_error: Option<String>,
    /// When the endpoint accepted the delivery.
    pub delivered_at: Option<DateTime<Utc>>,
    /// When the delivery was created, which is when its event happened.
    pub created_at: DateTime<Utc>,
    /// When the delivery was last attempted.
    pub updated_at: DateTime<Utc>,
}

/// A delivery about to be written.
#[derive(Debug, Insertable)]
#[diesel(table_name = webhook_deliveries)]
pub(super) struct NewWebhookDelivery<'a> {
    pub webhook_endpoint_id: Uuid,
    pub event_type: &'a str,
    pub payload: &'a JsonValue,
}

/// The background job that delivers one row.
///
/// The payload is the delivery's id and nothing else, because by the time a
/// worker reaches it the row is the only current view of what to send and
/// where. Retries ride the queue's own backoff; see `docs/jobs.md`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeliverWebhook {
    /// The `webhook_deliveries` row to send.
    pub delivery_id: Uuid,
}

impl Job for DeliverWebhook {
    /// Namespaced, because a `KIND` is data shared with every row already
    /// enqueued and an application's own job must never collide with it.
    const KIND: &'static str = "anubis.webhooks.deliver";
    /// Its own queue: a customer endpoint that hangs for ten seconds must not
    /// hold up an application's own work.
    const QUEUE: &'static str = QUEUE;
    const MAX_ATTEMPTS: i32 = 5;
}

/// The queue outgoing deliveries run on.
///
/// Named here as well as on the job so an application can give it a worker of
/// its own, with its own concurrency, without repeating a string literal.
pub const QUEUE: &str = "webhooks";

/// Sends deliveries: the handler side of [`DeliverWebhook`].
///
/// Register it on a worker and the framework owns the rest of the delivery
/// path:
///
/// ```ignore
/// let deliverer = anubis::webhooks::Deliverer::new(pool.clone(), &config);
/// let worker = anubis::jobs::Worker::builder(pool)
///     .register(move |job: anubis::webhooks::DeliverWebhook| {
///         let deliverer = deliverer.clone();
///         async move { deliverer.deliver(job).await }
///     })
///     .build();
/// ```
///
/// Cloning is cheap: the pool and the HTTP client are handles, and the key is
/// 32 bytes.
#[derive(Clone)]
pub struct Deliverer {
    pool: DbPool,
    http: reqwest::Client,
    key: SecretKey,
    /// Whether a stored `http://` URL may still be delivered to.
    allow_insecure: bool,
}

impl Deliverer {
    /// Builds a deliverer that draws deliveries from `pool`.
    ///
    /// The configuration supplies the signing key and decides whether plain
    /// `http://` endpoints may be delivered to, which is the same rule that
    /// governs whether one may be subscribed in the first place.
    ///
    /// # Panics
    /// Panics if the HTTP client cannot be built, which means the process has
    /// no usable TLS backend and nothing it does over the network would work.
    #[must_use]
    pub fn new(pool: DbPool, config: &AppConfig) -> Self {
        Self {
            pool,
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                // A redirect would carry a body signed for the first host to a
                // second one the team never subscribed.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("the HTTP client must build"),
            key: config.secret_key.clone(),
            allow_insecure: !config.environment.is_production(),
        }
    }

    /// Delivers one row, recording the outcome on it either way.
    ///
    /// # Errors
    /// Returns the failure that was recorded, so the queue retries the job on
    /// its own backoff. A row that is already `delivered` returns `Ok` without
    /// sending anything, which is what makes an at-least-once redelivery of the
    /// same job harmless.
    pub async fn deliver(&self, job: DeliverWebhook) -> Result<(), BoxError> {
        let mut connection = self.pool.get().await?;

        let Some((delivery, endpoint)) = load(&mut connection, job.delivery_id).await? else {
            // The endpoint was deleted, taking its deliveries with it. There is
            // nothing left to send and nothing to record.
            tracing::info!(
                webhook.delivery.id = %job.delivery_id,
                "the delivery is gone; its endpoint was deleted",
            );
            return Ok(());
        };

        if delivery.status == DeliveryStatus::Delivered.as_str() {
            return Ok(());
        }

        let attempts = delivery.attempts.saturating_add(1);
        let outcome = self.attempt(&mut connection, &delivery, &endpoint).await;

        match outcome {
            Ok(response_status) => {
                record(
                    &mut connection,
                    delivery.id,
                    Outcome {
                        status: DeliveryStatus::Delivered,
                        attempts,
                        response_status: Some(response_status),
                        last_error: None,
                        delivered_at: Some(Utc::now()),
                    },
                )
                .await?;
                tracing::info!(
                    webhook.delivery.id = %delivery.id,
                    webhook.event = %delivery.event_type,
                    http.response.status_code = response_status,
                    "delivered {{webhook.event}} answering {{http.response.status_code}}",
                );
                Ok(())
            }
            Err(failure) => {
                let exhausted = attempts >= DeliverWebhook::MAX_ATTEMPTS;
                record(
                    &mut connection,
                    delivery.id,
                    Outcome {
                        status: if exhausted {
                            DeliveryStatus::Dead
                        } else {
                            DeliveryStatus::Failed
                        },
                        attempts,
                        response_status: failure.response_status,
                        last_error: Some(failure.message.clone()),
                        delivered_at: None,
                    },
                )
                .await?;
                if exhausted {
                    announce_dead(&mut connection, &delivery, &endpoint).await?;
                }
                Err(failure.message.into())
            }
        }
    }

    /// Signs and sends one request, returning the status a `2xx` answered with.
    async fn attempt(
        &self,
        connection: &mut AsyncPgConnection,
        delivery: &WebhookDelivery,
        endpoint: &endpoint::WebhookEndpoint,
    ) -> Result<i32, Failure> {
        // Re-checked at send time, not only at subscribe time: the rule depends
        // on the environment, and an endpoint subscribed in development must
        // not become a plaintext request when the same database is promoted.
        endpoint::validate_url(&endpoint.url, self.allow_insecure).map_err(Failure::of)?;

        let secret = endpoint::signing_secret(connection, &self.key, endpoint.id)
            .await
            .map_err(Failure::of)?
            .ok_or_else(|| {
                Failure::of("the endpoint's signing secret could not be opened; recreate it")
            })?;

        // Serialized once and sent verbatim, because the signature covers these
        // exact bytes and a re-serialization is free to reorder them.
        let body = serde_json::to_string(&delivery.payload).map_err(Failure::of)?;
        let timestamp = Utc::now().timestamp();

        let response = self
            .http
            .post(&endpoint.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(signature::ID_HEADER, delivery.id.to_string())
            .header(signature::EVENT_HEADER, &delivery.event_type)
            .header(signature::TIMESTAMP_HEADER, timestamp.to_string())
            .header(
                signature::SIGNATURE_HEADER,
                signature::sign(&secret, timestamp, &body),
            )
            .body(body)
            .send()
            .await
            .map_err(Failure::of)?;

        let status = i32::from(response.status().as_u16());
        if response.status().is_success() {
            Ok(status)
        } else {
            Err(Failure {
                message: format!("the endpoint answered {status}"),
                response_status: Some(status),
            })
        }
    }
}

impl Debug for Deliverer {
    /// Written by hand rather than derived: the connection pool is not `Debug`,
    /// and the signing key must never render itself anyway.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deliverer")
            .field("timeout", &REQUEST_TIMEOUT)
            .field("allow_insecure", &self.allow_insecure)
            .finish_non_exhaustive()
    }
}

/// What one failed attempt has to say for itself.
struct Failure {
    message: String,
    /// Present only when the endpoint answered at all.
    response_status: Option<i32>,
}

impl Failure {
    /// A failure with no HTTP answer behind it: DNS, TLS, a timeout, a refusal.
    ///
    /// The message is capped at [`MAX_ERROR_LENGTH`], on a character boundary,
    /// because a client error can quote an arbitrarily long URL or body.
    fn of(message: impl Display) -> Self {
        Self {
            message: message.to_string().chars().take(MAX_ERROR_LENGTH).collect(),
            response_status: None,
        }
    }
}

/// The columns one attempt writes back.
struct Outcome {
    status: DeliveryStatus,
    attempts: i32,
    response_status: Option<i32>,
    last_error: Option<String>,
    delivered_at: Option<DateTime<Utc>>,
}

/// Loads a delivery together with the endpoint it is addressed to.
async fn load(
    connection: &mut AsyncPgConnection,
    delivery_id: Uuid,
) -> QueryResult<Option<(WebhookDelivery, endpoint::WebhookEndpoint)>> {
    webhook_deliveries::table
        .inner_join(webhook_endpoints::table)
        .filter(webhook_deliveries::id.eq(delivery_id))
        .select((
            WebhookDelivery::as_select(),
            endpoint::WebhookEndpoint::as_select(),
        ))
        .first(connection)
        .await
        .optional()
}

/// Tells a team's admins that one of their endpoints stopped answering.
///
/// Sent once per endpoint per [`DEAD_NOTICE_WINDOW`], not once per delivery: a
/// receiver that is down fails everything sent to it, and an inbox holding one
/// notice per lost event would say less than a single one does.
///
/// The endpoint is left active. Pausing a team's subscription on the
/// framework's initiative would lose events nobody asked it to lose, so the
/// team is told and the decision stays theirs; the delivery log on the
/// Developers screen is what the notice points at.
async fn announce_dead(
    connection: &mut AsyncPgConnection,
    delivery: &WebhookDelivery,
    endpoint: &endpoint::WebhookEndpoint,
) -> Result<(), BoxError> {
    let recent_notice: i64 = webhook_deliveries::table
        .filter(webhook_deliveries::webhook_endpoint_id.eq(endpoint.id))
        .filter(webhook_deliveries::id.ne(delivery.id))
        .filter(webhook_deliveries::status.eq(DeliveryStatus::Dead.as_str()))
        .filter(
            webhook_deliveries::updated_at
                .gt(Utc::now() - TimeDelta::hours(DEAD_NOTICE_WINDOW_HOURS)),
        )
        .count()
        .get_result(connection)
        .await?;
    if recent_notice > 0 {
        return Ok(());
    }

    let admins = crate::notifications::team_admins(connection, endpoint.team_id).await?;
    if admins.is_empty() {
        return Ok(());
    }

    let title = "A webhook endpoint stopped answering".to_owned();
    let body = format!(
        "{} could not be delivered to {} after {} attempts.",
        delivery.event_type,
        endpoint.url,
        DeliverWebhook::MAX_ATTEMPTS,
    );
    let href = crate::notifications::team_developers_href(endpoint.team_id);

    // One transaction for the whole audience, so every admin is told or none
    // is, and the pings commit with the rows they announce.
    connection
        .transaction::<(), diesel::result::Error, _>(async |transaction| {
            for admin in admins {
                crate::notifications::notify(
                    transaction,
                    crate::notifications::NewNotification {
                        user_id: admin,
                        team_id: Some(endpoint.team_id),
                        kind: crate::notifications::WEBHOOK_DELIVERY_FAILED,
                        title: &title,
                        body: Some(&body),
                        href: Some(&href),
                    },
                )
                .await?;
            }
            Ok(())
        })
        .await?;

    Ok(())
}

/// Writes one attempt's outcome onto its row.
async fn record(
    connection: &mut AsyncPgConnection,
    delivery_id: Uuid,
    outcome: Outcome,
) -> QueryResult<()> {
    diesel::update(webhook_deliveries::table.find(delivery_id))
        .set((
            webhook_deliveries::status.eq(outcome.status.as_str()),
            webhook_deliveries::attempts.eq(outcome.attempts),
            webhook_deliveries::response_status.eq(outcome.response_status),
            webhook_deliveries::last_error.eq(outcome.last_error),
            webhook_deliveries::delivered_at.eq(outcome.delivered_at),
        ))
        .execute(connection)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DeliverWebhook, DeliveryStatus, QUEUE};
    use crate::jobs::Job;

    #[test]
    fn statuses_render_as_the_values_stored_and_served() {
        assert_eq!(DeliveryStatus::Pending.as_str(), "pending");
        assert_eq!(DeliveryStatus::Delivered.to_string(), "delivered");
        assert_eq!(DeliveryStatus::Failed.as_str(), "failed");
        assert_eq!(DeliveryStatus::Dead.as_str(), "dead");
    }

    #[test]
    fn the_job_is_namespaced_and_runs_on_its_own_queue() {
        assert!(DeliverWebhook::KIND.starts_with("anubis."));
        assert_eq!(DeliverWebhook::QUEUE, QUEUE);
        assert_ne!(DeliverWebhook::QUEUE, crate::jobs::DEFAULT_QUEUE);
    }
}
