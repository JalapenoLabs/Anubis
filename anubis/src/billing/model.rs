//! The subscription an organization holds, and the statuses it moves through.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::schema::subscriptions;

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
    /// The terminal statuses are the ones the partial unique index in the
    /// billing migration excludes, so an organization may hold any number of
    /// finished subscriptions and only one that is still running. Change this
    /// set and change that index in the same migration.
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

/// An organization's subscription, mirroring Stripe's record of it.
///
/// The free plan is the absence of a row; see [`crate::billing`].
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Subscription {
    /// Primary key.
    pub id: Uuid,
    /// The organization being billed.
    pub organization_id: Uuid,
    /// The `config/billing.yml` plan this subscription buys.
    pub plan_key: String,
    /// Stripe's id for the subscription, e.g. `sub_1Q...`.
    pub stripe_subscription_id: String,
    /// One of [`SubscriptionStatus`]'s values.
    pub status: String,
    /// Which of the plan's prices was bought: `monthly` or `yearly`.
    pub billing_interval: String,
    /// Seats bought, which is the line item's quantity.
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
    /// The organization's subscription that is not over, if it has one.
    ///
    /// "Not over" is [`SubscriptionStatus::is_terminal`] inverted, which is the
    /// same condition the unique index enforces, so this can only ever match
    /// one row.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn current_for_organization(
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<Option<Self>> {
        let terminal: Vec<&str> = SubscriptionStatus::ALL
            .into_iter()
            .filter(|status| status.is_terminal())
            .map(SubscriptionStatus::as_str)
            .collect();

        subscriptions::table
            .filter(subscriptions::organization_id.eq(organization_id))
            .filter(subscriptions::status.ne_all(terminal))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
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

    /// Whether this subscription's plan is in force right now.
    #[must_use]
    pub fn grants_access(&self) -> bool {
        self.status().is_some_and(SubscriptionStatus::grants_access)
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
