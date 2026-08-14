//! The `HypotheticalSenderWebhook` model: one request Hypothetical Sender sent.
//!
//! A row is written the moment a request arrives and is never rewritten, only
//! stamped: `processed_at` when its job succeeds, `error` when its job fails.
//! That makes the table the receiving half's whole history, which is what a
//! developer needs when a provider insists it sent something the application
//! cannot find.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::schema::hypothetical_sender_webhooks;

/// One webhook received from Hypothetical Sender.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable)]
#[diesel(table_name = hypothetical_sender_webhooks)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HypotheticalSenderWebhook {
    /// Primary key.
    pub id: Uuid,
    /// The provider's JSON, exactly as it arrived.
    pub payload: JsonValue,
    /// The request headers, minus the ones that would be credentials.
    pub headers: JsonValue,
    /// Whether the signature checked out when the request arrived.
    pub verified: bool,
    /// When the request arrived, which is when this row was created.
    pub received_at: DateTime<Utc>,
    /// When processing finished, or `None` while the row is still waiting.
    pub processed_at: Option<DateTime<Utc>>,
    /// The most recent processing failure.
    pub error: Option<String>,
    /// When the row was last stamped, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = hypothetical_sender_webhooks)]
struct NewHypotheticalSenderWebhook<'a> {
    payload: &'a JsonValue,
    headers: &'a JsonValue,
    verified: bool,
}

impl HypotheticalSenderWebhook {
    /// Stores one received request and returns the row it became.
    ///
    /// The endpoint calls this inside a transaction that also enqueues the
    /// processing job, so a stored webhook always has work queued for it and a
    /// queued job always has a row to read.
    ///
    /// # Errors
    /// Returns the underlying query error when the insert fails.
    pub async fn store(
        connection: &mut AsyncPgConnection,
        payload: &JsonValue,
        headers: &JsonValue,
        verified: bool,
    ) -> QueryResult<Self> {
        diesel::insert_into(hypothetical_sender_webhooks::table)
            .values(NewHypotheticalSenderWebhook {
                payload,
                headers,
                verified,
            })
            .returning(Self::as_returning())
            .get_result(connection)
            .await
    }

    /// Loads one stored webhook by id.
    ///
    /// The processing job carries an id and nothing else, because by the time a
    /// worker reaches it the row is the only current view of what arrived.
    ///
    /// # Errors
    /// Returns [`diesel::result::Error::NotFound`] when no such row exists, and
    /// the underlying query error otherwise.
    pub async fn find(connection: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Self> {
        hypothetical_sender_webhooks::table
            .filter(hypothetical_sender_webhooks::id.eq(id))
            .select(Self::as_select())
            .first(connection)
            .await
    }

    /// Marks one webhook processed, clearing any earlier failure.
    ///
    /// # Errors
    /// Returns the underlying query error when the update fails.
    pub async fn mark_processed(
        connection: &mut AsyncPgConnection,
        id: Uuid,
    ) -> QueryResult<usize> {
        diesel::update(hypothetical_sender_webhooks::table.find(id))
            .set((
                hypothetical_sender_webhooks::processed_at.eq(Utc::now()),
                hypothetical_sender_webhooks::error.eq(None::<String>),
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
    pub async fn mark_failed(
        connection: &mut AsyncPgConnection,
        id: Uuid,
        error: &str,
    ) -> QueryResult<usize> {
        diesel::update(hypothetical_sender_webhooks::table.find(id))
            .set(hypothetical_sender_webhooks::error.eq(error))
            .execute(connection)
            .await
    }
}
