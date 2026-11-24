//! The subscriptions an organization holds, their line items, and their statuses.
//!
//! An organization holds at most one live **plan subscription**, the one that
//! names no [`Subject`] and buys a plan from `config/billing.yml`, and any
//! number of live **subject subscriptions**, each paying for one of the
//! application's own records. The database enforces both halves with partial
//! unique indexes: one live plan subscription per organization, one live
//! subscription per subject.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::schema::{subscription_items, subscriptions};

/// Where a subscription stands, in Stripe's own vocabulary.
///
/// Stored verbatim rather than mapped onto a smaller set of our own, because
/// the distinctions are the ones support is asked about: `past_due` is a card
/// that failed and a customer who still has access, `unpaid` is the same
/// customer after Stripe gave up retrying, and `incomplete` is a purchase that
/// never finished its first payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubscriptionStatus {
    /// The first payment has not completed yet.
    Incomplete,
    /// The first payment never completed, and Stripe closed it out.
    IncompleteExpired,
    /// Inside a trial; nothing has been charged yet.
    Trialing,
    /// Paid and current.
    Active,
    /// A payment failed and Stripe is still retrying.
    PastDue,
    /// Over. Nothing further will be charged.
    Canceled,
    /// Stripe stopped retrying a failed payment.
    Unpaid,
    /// Paused, typically at the end of a trial with no payment method.
    Paused,
}

impl SubscriptionStatus {
    /// Every status, in the order a subscription tends to meet them.
    pub const ALL: [Self; 8] = [
        Self::Incomplete,
        Self::IncompleteExpired,
        Self::Trialing,
        Self::Active,
        Self::PastDue,
        Self::Canceled,
        Self::Unpaid,
        Self::Paused,
    ];

    /// The value stored in the `status` column, as Stripe spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::IncompleteExpired => "incomplete_expired",
            Self::Trialing => "trialing",
            Self::Active => "active",
            Self::PastDue => "past_due",
            Self::Canceled => "canceled",
            Self::Unpaid => "unpaid",
            Self::Paused => "paused",
        }
    }

    /// Reads a status off a Stripe object or a stored row.
    ///
    /// # Examples
    /// ```
    /// use anubis::billing::SubscriptionStatus;
    ///
    /// assert_eq!(
    ///     SubscriptionStatus::parse("past_due"),
    ///     Some(SubscriptionStatus::PastDue),
    /// );
    /// assert_eq!(SubscriptionStatus::parse("lapsed"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }

    /// Whether the subscription is over for good.
    ///
    /// The terminal statuses are the ones the two partial unique indexes on
    /// `subscriptions` exclude, so an organization may hold any number of
    /// finished subscriptions and only one running plan subscription, and a
    /// subject only one running subscription. Change this set and change both
    /// indexes in the same migration.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Canceled | Self::IncompleteExpired)
    }

    /// Whether the subscription's plan is in force right now.
    ///
    /// `past_due` counts: Stripe is still retrying the card, and cutting a
    /// paying customer off during a retry window is how a failed payment turns
    /// into a cancelled account. An organization whose subscription does not
    /// grant access falls back to the free plan.
    #[must_use]
    pub fn grants_access(self) -> bool {
        matches!(self, Self::Trialing | Self::Active | Self::PastDue)
    }
}

/// What a subject subscription pays for: one of the application's own records.
///
/// A model name and a row id, the same pair the audit log names a subject
/// with, stored as `subject_type` and `subject_id`. The framework never reads
/// the record itself: it lives in a table the application owns, and whether a
/// subject belongs to an organization is the application's to decide before it
/// asks for a subscription.
///
/// # Examples
/// ```
/// use anubis::billing::Subject;
/// use uuid::Uuid;
///
/// let server = Subject::new("Server", Uuid::nil());
/// assert_eq!(server.kind(), "Server");
/// assert_eq!(server.to_string(), "Server 00000000-0000-0000-0000-000000000000");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject {
    kind: String,
    id: Uuid,
}

impl Subject {
    /// Names one record: its model, as the audit log spells it, and its id.
    #[must_use]
    pub fn new(kind: impl Into<String>, id: Uuid) -> Self {
        Self {
            kind: kind.into(),
            id,
        }
    }

    /// The model the record belongs to, stored as `subject_type`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The record's id, stored as `subject_id`.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }
}

impl Display for Subject {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind, self.id)
    }
}

/// An organization's subscription, mirroring Stripe's record of it.
///
/// A plan subscription names its plan and no subject; a subject subscription
/// names its subject and no plan. The free plan is the absence of a live plan
/// subscription; see [`crate::billing`].
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Subscription {
    /// Primary key.
    pub id: Uuid,
    /// The organization being billed.
    pub organization_id: Uuid,
    /// The `config/billing.yml` plan a plan subscription buys; `None` on a
    /// subject subscription, which buys its subject instead.
    pub plan_key: Option<String>,
    /// The model of the record a subject subscription pays for; `None` on the
    /// plan subscription.
    pub subject_type: Option<String>,
    /// The id of that record; `None` on the plan subscription.
    pub subject_id: Option<Uuid>,
    /// Stripe's id for the subscription, e.g. `sub_1Q...`.
    pub stripe_subscription_id: String,
    /// One of [`SubscriptionStatus`]'s values.
    pub status: String,
    /// How often it bills: `monthly` or `yearly`.
    pub billing_interval: String,
    /// Seats bought, which is the plan's line item's quantity. One on a
    /// subject subscription, whose quantities are on its items.
    pub quantity: i32,
    /// When the paid-for period ends.
    pub current_period_end: Option<DateTime<Utc>>,
    /// Whether the subscription stops at the end of that period.
    pub cancel_at_period_end: bool,
    /// When the subscription was first recorded here.
    pub created_at: DateTime<Utc>,
    /// When it was last updated, by an event or by a purchase.
    pub updated_at: DateTime<Utc>,
}

impl Subscription {
    /// The organization's live plan subscription, if it has one.
    ///
    /// "Live" is [`SubscriptionStatus::is_terminal`] inverted, and the plan
    /// subscription is the one naming no subject: the same condition the
    /// plan index enforces, so this can only ever match one row.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn current_for_organization(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<Option<Self>> {
        subscriptions::table
            .filter(subscriptions::organization_id.eq(organization_id))
            .filter(subscriptions::subject_type.is_null())
            .filter(subscriptions::status.ne_all(terminal_statuses()))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }

    /// Every live subscription the organization holds, plan subscription first.
    ///
    /// The subject subscriptions follow, grouped by model and oldest first, so
    /// a screen listing them reads in a stable order.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn live_for_organization(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<Vec<Self>> {
        subscriptions::table
            .filter(subscriptions::organization_id.eq(organization_id))
            .filter(subscriptions::status.ne_all(terminal_statuses()))
            .order((
                subscriptions::subject_type.asc().nulls_first(),
                subscriptions::created_at.asc(),
            ))
            .select(Self::as_select())
            .load(connection)
            .await
    }

    /// The live subscription paying for `subject`, when the organization holds one.
    ///
    /// Scoped to the organization as well as the subject, so a subject another
    /// organization pays for reads as absent rather than as somebody else's.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn live_for_subject(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        subject: &Subject,
    ) -> QueryResult<Option<Self>> {
        subscriptions::table
            .filter(subscriptions::organization_id.eq(organization_id))
            .filter(subscriptions::subject_type.eq(subject.kind()))
            .filter(subscriptions::subject_id.eq(subject.id()))
            .filter(subscriptions::status.ne_all(terminal_statuses()))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }

    /// Whether a live subscription paying for one of the organization's records grants access.
    ///
    /// What makes an organization count as paying for something without a
    /// plan subscription; see [`crate::billing::limits`]. One `EXISTS`, so an
    /// organization paying for hundreds of records is asked about one row.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn pays_for_records(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<bool> {
        diesel::select(diesel::dsl::exists(
            subscriptions::table
                .filter(subscriptions::organization_id.eq(organization_id))
                .filter(subscriptions::subject_type.is_not_null())
                .filter(subscriptions::status.eq_any(granting_statuses())),
        ))
        .get_result(connection)
        .await
    }

    /// The record this subscription pays for, or `None` for the plan subscription.
    #[must_use]
    pub fn subject(&self) -> Option<Subject> {
        self.subject_type
            .as_deref()
            .zip(self.subject_id)
            .map(|(kind, id)| Subject::new(kind, id))
    }

    /// The status as a value, or `None` for a row Stripe has outgrown.
    ///
    /// A status this framework version does not know is not a reason to fail a
    /// page: it reads as "no access", the plan falls back to free, and the log
    /// says which word was unfamiliar.
    #[must_use]
    pub fn status(&self) -> Option<SubscriptionStatus> {
        SubscriptionStatus::parse(&self.status)
    }

    /// Whether this subscription is in force right now.
    #[must_use]
    pub fn grants_access(&self) -> bool {
        self.status().is_some_and(SubscriptionStatus::grants_access)
    }
}

/// The statuses a live subscription is not in, as the column stores them.
///
/// The same list the two partial unique indexes exclude, read from
/// [`SubscriptionStatus::is_terminal`] so the queries and the predicate agree.
pub(super) fn terminal_statuses() -> Vec<&'static str> {
    SubscriptionStatus::ALL
        .into_iter()
        .filter(|status| status.is_terminal())
        .map(SubscriptionStatus::as_str)
        .collect()
}

/// The statuses a subscription grants access in, as the column stores them.
///
/// Read from [`SubscriptionStatus::grants_access`], so the query and the
/// predicate agree.
fn granting_statuses() -> Vec<&'static str> {
    SubscriptionStatus::ALL
        .into_iter()
        .filter(|status| status.grants_access())
        .map(SubscriptionStatus::as_str)
        .collect()
}

/// One line item of a subscription: a Stripe price, and how many of it.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = subscription_items)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SubscriptionItem {
    /// Primary key.
    pub id: Uuid,
    /// The subscription the item belongs to.
    pub subscription_id: Uuid,
    /// Stripe's id for the item, e.g. `si_1Q...`.
    pub stripe_item_id: String,
    /// The Stripe price the item bills.
    pub stripe_price_id: String,
    /// How many of the price; `None` for a metered price, which bills usage.
    pub quantity: Option<i32>,
    /// When the item was first recorded here.
    pub created_at: DateTime<Utc>,
    /// When it was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A subscription and its line items, which is how the billing API serves one.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionWithItems {
    /// The subscription, its fields inlined into the same JSON object.
    #[serde(flatten)]
    pub subscription: Subscription,
    /// Its line items, oldest first.
    pub items: Vec<SubscriptionItem>,
}

impl SubscriptionWithItems {
    /// Attaches each subscription's line items, in one query for all of them.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn load(
        connection: &mut AsyncPgConnection,
        subscriptions: Vec<Subscription>,
    ) -> QueryResult<Vec<Self>> {
        let ids: Vec<Uuid> = subscriptions
            .iter()
            .map(|subscription| subscription.id)
            .collect();
        let items: Vec<SubscriptionItem> = subscription_items::table
            .filter(subscription_items::subscription_id.eq_any(&ids))
            .order((
                subscription_items::created_at.asc(),
                subscription_items::stripe_item_id.asc(),
            ))
            .select(SubscriptionItem::as_select())
            .load(connection)
            .await?;

        let mut by_subscription: BTreeMap<Uuid, Vec<SubscriptionItem>> = BTreeMap::new();
        for item in items {
            by_subscription
                .entry(item.subscription_id)
                .or_default()
                .push(item);
        }

        Ok(subscriptions
            .into_iter()
            .map(|subscription| {
                let items = by_subscription.remove(&subscription.id).unwrap_or_default();
                Self {
                    subscription,
                    items,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::SubscriptionStatus;

    #[test]
    fn statuses_round_trip_through_stripes_spelling() {
        for status in SubscriptionStatus::ALL {
            assert_eq!(
                SubscriptionStatus::parse(status.as_str()),
                Some(status),
                "{status:?} must round trip",
            );
        }
        assert_eq!(SubscriptionStatus::parse("lapsed"), None);
        assert_eq!(SubscriptionStatus::PastDue.as_str(), "past_due");
    }

    #[test]
    fn only_finished_subscriptions_are_terminal() {
        for status in SubscriptionStatus::ALL {
            let expected = matches!(
                status,
                SubscriptionStatus::Canceled | SubscriptionStatus::IncompleteExpired
            );
            assert_eq!(status.is_terminal(), expected, "for {status:?}");
        }
    }

    #[test]
    fn access_survives_a_failing_card_but_not_a_cancellation() {
        assert!(SubscriptionStatus::Active.grants_access());
        assert!(SubscriptionStatus::Trialing.grants_access());
        assert!(
            SubscriptionStatus::PastDue.grants_access(),
            "Stripe is still retrying the card",
        );

        for status in [
            SubscriptionStatus::Incomplete,
            SubscriptionStatus::IncompleteExpired,
            SubscriptionStatus::Canceled,
            SubscriptionStatus::Unpaid,
            SubscriptionStatus::Paused,
        ] {
            assert!(!status.grants_access(), "{status:?} must not grant access");
        }
    }
}
