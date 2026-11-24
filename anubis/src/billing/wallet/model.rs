//! The wallet row, its ledger entries, and the queries both are read with.
//!
//! Writes go through [`super::ledger`], which holds the wallet's row lock for
//! each one; everything here only reads.

use std::fmt::{self, Display, Formatter};

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::http::ListParams;
use crate::schema::{wallet_entries, wallets};

/// What one ledger entry records.
///
/// Credits are positive and debits negative, which the table holds as a check
/// constraint per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Credit bought through Stripe Checkout.
    TopUp,
    /// Credit bought off-session with the saved card, when the balance fell
    /// below the auto-recharge threshold.
    Recharge,
    /// The application charged for usage.
    Spend,
    /// Stripe refunded a payment that bought credit.
    Refund,
    /// A refund failed or was cancelled after it was debited, so its credit
    /// came back.
    RefundReversal,
}

impl EntryKind {
    /// Every kind, in the order a wallet tends to meet them.
    pub const ALL: [Self; 5] = [
        Self::TopUp,
        Self::Recharge,
        Self::Spend,
        Self::Refund,
        Self::RefundReversal,
    ];

    /// The value stored in the `kind` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopUp => "top_up",
            Self::Recharge => "recharge",
            Self::Spend => "spend",
            Self::Refund => "refund",
            Self::RefundReversal => "refund_reversal",
        }
    }

    /// Reads a kind off a stored row.
    ///
    /// # Examples
    /// ```
    /// use anubis::billing::EntryKind;
    ///
    /// assert_eq!(EntryKind::parse("top_up"), Some(EntryKind::TopUp));
    /// assert_eq!(EntryKind::parse("bonus"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

impl Display for EntryKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One organization's wallet, as the `wallets` row holds it.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = wallets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Wallet {
    /// The organization the wallet belongs to, which is its key.
    pub organization_id: Uuid,
    /// The balance in micro-units: the sum of the organization's ledger.
    pub balance_micros: i64,
    /// The balance below which a spend arms a recharge, when auto-recharge is on.
    pub auto_recharge_threshold_micros: Option<i64>,
    /// What a recharge charges the saved card, when auto-recharge is on.
    pub auto_recharge_amount_micros: Option<i64>,
    /// The card a top-up saved for charging off-session, e.g. `pm_1Q...`.
    pub recharge_payment_method_id: Option<String>,
    /// When the payment that saved that card was created at Stripe.
    pub recharge_payment_method_saved_at: Option<DateTime<Utc>>,
    /// The recharge in flight, if one is armed.
    pub recharge_attempt_id: Option<Uuid>,
    /// What the recharge in flight charges.
    pub recharge_attempt_amount_micros: Option<i64>,
    /// Moves when Stripe saved an unusable answer under the attempt's key.
    pub recharge_attempt_revision: i32,
    /// When the recharge in flight was armed, which bounds the search for its payment.
    pub recharge_armed_at: Option<DateTime<Utc>>,
    /// When the recharge in flight's job was last queued.
    pub recharge_queued_at: Option<DateTime<Utc>>,
    /// When the last recharge failed, turning auto-recharge off.
    pub recharge_failed_at: Option<DateTime<Utc>>,
    /// Why it failed, in Stripe's words, for an operator.
    pub recharge_failure: Option<String>,
    /// When the wallet was created, by its first credit.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl Wallet {
    /// The auto-recharge settings, `(threshold, amount)`, when auto-recharge is on.
    #[must_use]
    pub fn auto_recharge(&self) -> Option<(i64, i64)> {
        self.auto_recharge_threshold_micros
            .zip(self.auto_recharge_amount_micros)
    }

    /// The organization's wallet, if it has one, read without a lock.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn find(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<Option<Self>> {
        wallets::table
            .find(organization_id)
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }
}

/// One line of the ledger.
///
/// Serialized as the billing API answers with it: every amount in
/// micro-units, the unit spelled out in the field name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Queryable, Selectable)]
#[diesel(table_name = wallet_entries)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WalletEntry {
    /// Primary key.
    pub id: Uuid,
    /// The organization whose wallet this is.
    pub organization_id: Uuid,
    /// Where the entry sits in the organization's ledger, oldest first.
    pub position: i64,
    /// One of [`EntryKind`]'s values.
    pub kind: String,
    /// Positive for a credit, negative for a debit.
    pub amount_micros: i64,
    /// The balance this entry left.
    pub balance_after_micros: i64,
    /// What a spend asked for, which exceeds what it took when the wallet ran dry.
    pub requested_micros: Option<i64>,
    /// The spend's own key, or the Stripe payment or refund the entry records.
    pub idempotency_key: String,
    /// The Stripe payment a credit or a refund belongs to.
    pub stripe_payment_intent_id: Option<String>,
    /// What a spend was for, in the application's words.
    pub description: Option<String>,
    /// The model of the record a spend paid for.
    pub subject_type: Option<String>,
    /// The id of that record.
    pub subject_id: Option<Uuid>,
    /// When the entry was written.
    pub created_at: DateTime<Utc>,
}

impl WalletEntry {
    /// The kind as a value; every stored row carries one the table allows.
    #[must_use]
    pub fn kind(&self) -> Option<EntryKind> {
        EntryKind::parse(&self.kind)
    }

    /// One page of an organization's ledger, newest first.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn page(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        params: &ListParams,
    ) -> QueryResult<Vec<Self>> {
        wallet_entries::table
            .filter(wallet_entries::organization_id.eq(organization_id))
            .order(wallet_entries::position.desc())
            .limit(params.limit())
            .offset(params.offset())
            .select(Self::as_select())
            .load(connection)
            .await
    }

    /// How many entries an organization's ledger holds.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn count(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<i64> {
        wallet_entries::table
            .filter(wallet_entries::organization_id.eq(organization_id))
            .count()
            .get_result(connection)
            .await
    }

    /// The entry of `kind` written under `key`, if there is one.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub(super) async fn find(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        kind: EntryKind,
        key: &str,
    ) -> QueryResult<Option<Self>> {
        wallet_entries::table
            .filter(wallet_entries::organization_id.eq(organization_id))
            .filter(wallet_entries::kind.eq(kind.as_str()))
            .filter(wallet_entries::idempotency_key.eq(key))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::EntryKind;

    #[test]
    fn kinds_round_trip_through_their_stored_spelling() {
        for kind in EntryKind::ALL {
            assert_eq!(EntryKind::parse(kind.as_str()), Some(kind), "{kind:?}");
        }
        assert_eq!(EntryKind::RefundReversal.as_str(), "refund_reversal");
    }
}
