//! Outgoing webhooks: a team subscribes an endpoint, and events reach it.
//!
//! This is the publishing half of Bullet Train's outgoing webhooks, rebuilt on
//! the Postgres job queue. A team creates an endpoint in the Developers
//! section, names the event types it wants, and receives a signed POST for
//! every matching event, carrying **the same serialized shape the REST API
//! answers with**. One serializer, three consumers: the account UI, `/api/v1`,
//! and this.
//!
//! # Emitting
//!
//! [`emit`] is the whole producer surface. Generated model handlers call it
//! from the functions both their account and their `/api/v1` halves share, so
//! a record created through the browser and one created through a bearer token
//! produce byte-identical events:
//!
//! ```ignore
//! let view = ProjectView::one(connection, record).await?;
//! anubis::webhooks::emit(connection, team_id, "project.created", &view).await?;
//! ```
//!
//! The insert and both enqueues ride the caller's own `connection`, which is
//! the point of keeping the queue in Postgres: emitting inside the transaction
//! that wrote the record means a rolled-back write sends nothing, and a
//! committed write never loses its event. See `docs/jobs.md`.
//!
//! # Event types
//!
//! An event type is `<model>.<action>` in snake case, the model singular:
//! `project.created`, `project.updated`, `project.destroyed`. The catalog is
//! the application's own models, so the framework validates the shape rather
//! than a list it cannot know; [`is_event_type`] is that check, and
//! [`LIFECYCLE_ACTIONS`] is the set of actions a scaffolded model emits.
//!
//! # Delivering
//!
//! Each subscribed endpoint gets a `pending` delivery row and one
//! [`DeliverWebhook`] job. A [`Deliverer`] registered on a worker POSTs the
//! payload, signs it (see [`signature`] for the exact scheme a receiver
//! verifies), and records the answer on the row. Failures retry on the queue's
//! backoff; a delivery that runs out of attempts is marked `dead` and stays
//! visible in the team's debugging screen.
//!
//! # Managing
//!
//! [`router`] serves the team-scoped subscription and debugging endpoints,
//! mounted alongside platform applications under `/developers`.
//!
//! # Receiving
//!
//! The other half of webhooks, receiving a third party's events, is generated
//! rather than mounted: `anubis scaffold webhook <Provider>` writes the table,
//! the endpoint, and the processing job into the application. The one piece it
//! borrows from here is
//! [`signature::verify_hmac_sha256`], the provider-agnostic comparison every
//! publisher's scheme ends in. See `docs/webhooks.md`.

mod delivery;
mod endpoint;
mod management;
pub mod signature;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::jobs;
use crate::schema::{webhook_deliveries, webhook_endpoints};

#[doc(inline)]
pub use delivery::{DeliverWebhook, Deliverer, DeliveryStatus, QUEUE, WebhookDelivery};
#[doc(inline)]
pub use endpoint::WebhookEndpoint;
#[doc(inline)]
pub use management::router;

/// The actions a scaffolded model emits, one per write it performs.
///
/// `destroyed` rather than `deleted`, matching Bullet Train and the framework's
/// own `Action::Destroy`, so the permission a write needs and the event it
/// produces are spelled the same way.
pub const LIFECYCLE_ACTIONS: [&str; 3] = ["created", "updated", "destroyed"];

/// Returns `true` when `candidate` is shaped like an event type.
///
/// The shape is `<model>.<action>`: a snake-case model name, a dot, and one of
/// [`LIFECYCLE_ACTIONS`]. The model half is not checked against anything,
/// because the catalog belongs to the application; what this catches is the
/// half the framework does define, so a subscription to `project.create` or
/// `Project.created` is refused at the point a person typed it rather than
/// silently receiving nothing forever.
///
/// # Examples
/// ```
/// assert!(anubis::webhooks::is_event_type("project.created"));
/// assert!(anubis::webhooks::is_event_type("applied_tag.destroyed"));
/// assert!(!anubis::webhooks::is_event_type("project.create"));
/// assert!(!anubis::webhooks::is_event_type("Project.created"));
/// ```
#[must_use]
pub fn is_event_type(candidate: &str) -> bool {
    let Some((model, action)) = candidate.split_once('.') else {
        return false;
    };

    let named = !model.is_empty()
        && model.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        && !model.starts_with('_')
        && !model.ends_with('_');

    named && LIFECYCLE_ACTIONS.contains(&action)
}

/// Emits `event_type` to every active endpoint of `team_id` that wants it.
///
/// Writes one `pending` delivery per subscribed endpoint and enqueues one
/// delivery job per row, all through `connection`, so emission commits with
/// the write that caused it. Returns how many endpoints were notified, which
/// is zero for a team that has subscribed nothing: emitting is cheap and
/// unconditional, and the subscription is what decides whether anything
/// happens.
///
/// `payload` is serialized once, and every endpoint receives those exact
/// bytes. Pass the same view the REST API answers with, so a receiver reading
/// the published OpenAPI document already knows the shape.
///
/// # Errors
/// Returns the underlying query error when a lookup or an insert fails, and a
/// [`diesel::result::Error::SerializationError`] when `payload` will not
/// render as JSON. Returning a query error is deliberate: the caller is
/// already inside a Diesel transaction, so the failure joins the rollback of
/// the write it belongs to rather than needing an error type of its own.
///
/// # Examples
/// ```ignore
/// connection
///     .transaction::<_, diesel::result::Error, _>(async |connection| {
///         let record = insert_project(connection).await?;
///         anubis::webhooks::emit(connection, team_id, "project.created", &record).await?;
///         Ok(record)
///     })
///     .await?;
/// ```
pub async fn emit(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    event_type: &str,
    payload: &impl Serialize,
) -> QueryResult<usize> {
    let subscribed: Vec<Uuid> = webhook_endpoints::table
        .filter(webhook_endpoints::team_id.eq(team_id))
        .filter(webhook_endpoints::active.eq(true))
        .filter(webhook_endpoints::event_types.contains(vec![event_type.to_owned()]))
        .select(webhook_endpoints::id)
        .load(connection)
        .await?;

    if subscribed.is_empty() {
        return Ok(0);
    }

    let payload = serde_json::to_value(payload)
        .map_err(|source| diesel::result::Error::SerializationError(Box::new(source)))?;
    let rows: Vec<delivery::NewWebhookDelivery<'_>> = subscribed
        .into_iter()
        .map(|webhook_endpoint_id| delivery::NewWebhookDelivery {
            webhook_endpoint_id,
            event_type,
            payload: &payload,
        })
        .collect();

    let queued: Vec<Uuid> = diesel::insert_into(webhook_deliveries::table)
        .values(&rows)
        .returning(webhook_deliveries::id)
        .get_results(connection)
        .await?;

    for delivery_id in &queued {
        jobs::enqueue_query(
            connection,
            &DeliverWebhook {
                delivery_id: *delivery_id,
            },
        )
        .await?;
    }

    Ok(queued.len())
}

#[cfg(test)]
mod tests {
    use super::{LIFECYCLE_ACTIONS, is_event_type};

    #[test]
    fn every_lifecycle_action_makes_a_valid_event_type() {
        for action in LIFECYCLE_ACTIONS {
            let event_type = format!("creative_concept.{action}");
            assert!(is_event_type(&event_type), "{event_type} must be valid");
        }
    }

    #[test]
    fn a_typo_in_either_half_is_refused() {
        for candidate in [
            "",
            "project",
            "project.",
            ".created",
            "project.create",
            "project.deleted",
            "Project.created",
            "project name.created",
            "project-name.created",
            "_project.created",
            "project_.created",
            "project.created.twice",
        ] {
            assert!(!is_event_type(candidate), "{candidate:?} must be refused");
        }
    }
}
