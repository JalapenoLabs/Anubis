//! The Stripe events this application has been told about.
//!
//! A row is written the moment a signed request arrives and is never rewritten,
//! only stamped: `processed_at` when its job succeeds, `error` when its job
//! fails. That makes the table the whole history of what Stripe said, which is
//! what an operator needs when Stripe's dashboard and this application disagree
//! about what a customer is paying for.
//!
//! Idempotency lives in the database rather than in the endpoint: Stripe
//! redelivers an event it did not hear a `2xx` for, sometimes while the first
//! delivery is still being processed, and a unique index on `stripe_event_id`
//! is the only thing that turns two concurrent redeliveries into one row and
//! one job.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::schema::stripe_billing_events;

/// One subscription event received from Stripe.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable)]
#[diesel(table_name = stripe_billing_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct StripeBillingEvent {
    /// Primary key.
    pub id: Uuid,
    /// Stripe's own id for the event, e.g. `evt_1Q...`.
    pub stripe_event_id: String,
    /// The event's type, e.g. `customer.subscription.updated`.
    pub event_type: String,
    /// Stripe's JSON, exactly as it arrived.
    pub payload: JsonValue,
    /// When Stripe created the event, which is how ordering is decided.
    pub stripe_created_at: Option<DateTime<Utc>>,
    /// When the request arrived, which is when this row was created.
    pub received_at: DateTime<Utc>,
    /// When processing finished, or `None` while the row is still waiting.
    pub processed_at: Option<DateTime<Utc>>,
    /// The most recent processing failure.
    pub error: Option<String>,
    /// When the row was last stamped, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// An event about to be stored; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = stripe_billing_events)]
pub(super) struct NewStripeBillingEvent<'a> {
    pub stripe_event_id: &'a str,
    pub event_type: &'a str,
    pub payload: &'a JsonValue,
    pub stripe_created_at: Option<DateTime<Utc>>,
}

impl StripeBillingEvent {
    /// Stores one event, or reports that it was already stored.
    ///
    /// Returns `None` when Stripe redelivered an event this application already
    /// holds, which is the receiver's whole idempotency guarantee: the caller
    /// then queues nothing and answers `200`, because the first delivery
    /// already has a job coming for it.
    ///
    /// # Errors
    /// Returns the underlying query error when the insert fails.
    pub(super) async fn store(
        connection: &mut AsyncPgConnection,
        event: NewStripeBillingEvent<'_>,
    ) -> QueryResult<Option<Self>> {
        diesel::insert_into(stripe_billing_events::table)
            .values(event)
            .on_conflict(stripe_billing_events::stripe_event_id)
            .do_nothing()
            .returning(Self::as_returning())
            .get_result(connection)
            .await
            .optional()
    }

    /// Loads one stored event by id.
    ///
    /// # Errors
    /// Returns [`diesel::result::Error::NotFound`] when no such row exists, and
    /// the underlying query error otherwise.
    pub async fn find(connection: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Self> {
        stripe_billing_events::table
            .find(id)
            .select(Self::as_select())
            .first(connection)
            .await
    }

    /// The row Stripe's own event id names, if this application holds it.
    ///
    /// # Errors
    /// Returns the underlying query error when the lookup fails.
    pub async fn find_by_stripe_id(
        connection: &mut AsyncPgConnection,
        stripe_event_id: &str,
    ) -> QueryResult<Option<Self>> {
        stripe_billing_events::table
            .filter(stripe_billing_events::stripe_event_id.eq(stripe_event_id))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }

    /// Marks one event processed, clearing any earlier failure.
    ///
    /// # Errors
    /// Returns the underlying query error when the update fails.
    pub(super) async fn mark_processed(
        connection: &mut AsyncPgConnection,
        id: Uuid,
    ) -> QueryResult<usize> {
        diesel::update(stripe_billing_events::table.find(id))
            .set((
                stripe_billing_events::processed_at.eq(Utc::now()),
                stripe_billing_events::error.eq(None::<String>),
            ))
            .execute(connection)
            .await
    }

    /// Records why processing failed, leaving the row unprocessed.
    ///
    /// The queue retries on its own backoff, so this is what the row looks like
    /// between attempts and what it is left holding if every attempt fails.
    ///
    /// # Errors
    /// Returns the underlying query error when the update fails.
    pub(super) async fn mark_failed(
        connection: &mut AsyncPgConnection,
        id: Uuid,
        error: &str,
    ) -> QueryResult<usize> {
        diesel::update(stripe_billing_events::table.find(id))
            .set(stripe_billing_events::error.eq(error))
            .execute(connection)
            .await
    }

    /// The object the event happened to: `data.object` in Stripe's envelope.
    ///
    /// Every Stripe event carries one, and which type it is follows from
    /// [`StripeBillingEvent::event_type`].
    #[must_use]
    pub fn object(&self) -> Option<&JsonValue> {
        self.payload.get("data")?.get("object")
    }
}
