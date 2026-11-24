//! Keeping the subscription projection in line with Stripe.
//!
//! Money lives at Stripe; the `subscriptions` and `subscription_items` tables
//! are the projection this application authorizes and renders from.
//! [`Reconciler`] is the only thing that writes them, and it has these ways in:
//!
//! - **Events**, the normal path. [`ProcessStripeEvent`] runs one stored event
//!   from `stripe_billing_events`, and the receiver
//!   [`webhook_router`](super::webhook_router) is what put it there.
//! - **Reconciliation**, the repair path. [`Reconciler::reconcile`] reads an
//!   organization's subscriptions out of Stripe and converges the local rows
//!   with what it finds, which is how a missed event stops mattering.
//! - **Seat synchronization**, the outbound path. [`SyncSeats`] runs through
//!   [`Reconciler::sync_seats`] after a membership changes, tells Stripe the
//!   new quantity of a per-seat price, and writes back what Stripe answers.
//! - **Application changes**, the other outbound path.
//!   [`Subscriptions`](super::Subscriptions) changes or cancels a subject
//!   subscription at Stripe and writes back the answer through the same
//!   convergence.
//!
//! The wallet's payments ride the same events and the same reconciliation:
//! a top-up, a recharge, or a refund names a payment, and
//! [`crate::billing::wallet`] reads it back and records it in the ledger
//! once. [`RechargeWallet`] runs through [`Reconciler::recharge_wallet`].
//!
//! A subscription and its line items are written together, in one
//! transaction behind one ordering guard, so an event too old to change a
//! subscription cannot change its items either, and the items written are
//! exactly the ones Stripe reported: an item Stripe no longer lists is deleted.
//!
//! # Reading rather than trusting
//!
//! An event's body is a snapshot of the moment Stripe created it, and a job may
//! process it minutes later, after a retry or a backlog. So a
//! `checkout.session.completed` and a `customer.subscription.created` or
//! `.updated` are treated as *notifications*: the event says which subscription
//! changed, and the framework reads that subscription back from Stripe before
//! writing anything.
//!
//! The one event trusted as it arrives is `customer.subscription.deleted`. A
//! cancelled subscription is over, and nothing Stripe could say about it later
//! would be newer, so re-reading it would spend a call to be told what the
//! event already said.
//!
//! # Events arrive out of order
//!
//! Stripe does not promise order, so a late `.updated` could otherwise undo a
//! cancellation that arrived first. Every write carries the creating event's
//! timestamp into `subscriptions.stripe_event_at`, and the upsert applies only
//! when the incoming event is not older than the one already applied. Two
//! events created in the same second both apply, which is safe because each
//! writes the state read back from Stripe rather than its own body.
//!
//! A write that read Stripe directly (reconciliation, seat synchronization,
//! an application's change) is stamped with the moment its read *began*, in
//! whole seconds; see [`read_began`]. Anything Stripe recorded before then is
//! in what it read, so an older event is rightly ignored, and anything later
//! may not be, so a newer event is free to apply. Letting an event win is
//! always safe, because processing one reads Stripe back. A write that did not
//! hold the subscription's row lock (reconciliation, seat synchronization) is
//! stamped a second earlier still, so it loses a tie with an event created in
//! the same second; see [`unlocked_read_began`]. And whatever the stamps say,
//! a subscription that is over is never written back to a live status, because
//! Stripe never revives one.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use diesel::prelude::*;
use diesel::upsert::excluded;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::StripeBillingEvent;
use super::limits;
use super::model::{Subject, Subscription, SubscriptionStatus, terminal_statuses};
use super::plans::{Interval, PlanSet};
use super::stripe::{
    self, CompletedCheckout, ItemUpdate, ORGANIZATION_METADATA, PLAN_METADATA, SUBJECT_ID_METADATA,
    SUBJECT_TYPE_METADATA, SubscriptionUpdate,
};
use super::wallet::RechargeWallet;
use super::wallet::payments::{self, Refunds};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::jobs::{BoxError, Job};
use crate::schema::{organizations, subscription_items, subscriptions};

/// The queue billing events are processed on.
///
/// Its own, rather than the application's default: Stripe replaying a day of
/// events, or a Stripe API call that hangs, must not hold up the work a person
/// is waiting on.
pub const QUEUE: &str = "billing";

/// The Stripe events the framework acts on.
///
/// This is the list to select when creating the endpoint in the Stripe
/// dashboard. Sending more than these is harmless: an event of any other type
/// is stored, marked processed, and ignored, because a receiver that failed on
/// unknown types would fail on every feature Stripe ships.
pub const HANDLED_EVENTS: [&str; 12] = [
    CHECKOUT_COMPLETED,
    SUBSCRIPTION_CREATED,
    SUBSCRIPTION_UPDATED,
    SUBSCRIPTION_DELETED,
    CHECKOUT_ASYNC_PAYMENT_SUCCEEDED,
    PAYMENT_SUCCEEDED,
    PAYMENT_FAILED,
    CHARGE_REFUNDED,
    CHARGE_REFUND_UPDATED,
    REFUND_CREATED,
    REFUND_UPDATED,
    REFUND_FAILED,
];

/// A purchase finished at Stripe Checkout; the subscription now exists.
const CHECKOUT_COMPLETED: &str = "checkout.session.completed";

/// A subscription was created, by checkout or in the Stripe dashboard.
const SUBSCRIPTION_CREATED: &str = "customer.subscription.created";

/// A subscription changed: its plan, its status, its quantity, its period.
const SUBSCRIPTION_UPDATED: &str = "customer.subscription.updated";

/// A subscription ended, and nothing further will be charged for it.
const SUBSCRIPTION_DELETED: &str = "customer.subscription.deleted";

/// A Checkout payment by a method that settles later, such as a bank debit, settled.
const CHECKOUT_ASYNC_PAYMENT_SUCCEEDED: &str = "checkout.session.async_payment_succeeded";

/// A payment succeeded: a top-up, or a recharge.
const PAYMENT_SUCCEEDED: &str = "payment_intent.succeeded";

/// A payment attempt failed, which ends a recharge.
const PAYMENT_FAILED: &str = "payment_intent.payment_failed";

/// A charge was refunded, in full or in part; its object is the charge.
const CHARGE_REFUNDED: &str = "charge.refunded";

/// A refund changed status, as older API versions announce it.
const CHARGE_REFUND_UPDATED: &str = "charge.refund.updated";

/// A refund was created; its object is the refund.
const REFUND_CREATED: &str = "refund.created";

/// A refund changed status.
const REFUND_UPDATED: &str = "refund.updated";

/// A refund failed, so its money came back.
const REFUND_FAILED: &str = "refund.failed";

/// The background job that processes one stored Stripe event.
///
/// The payload is the row's id and nothing else, because the row is the only
/// current view of what arrived by the time a worker reaches it.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessStripeEvent {
    /// The `stripe_billing_events` row to process.
    pub event_id: Uuid,
}

impl Job for ProcessStripeEvent {
    /// Namespaced, because a `KIND` is data shared with every row already
    /// enqueued and an application's own job must never collide with it.
    const KIND: &'static str = "anubis.billing.stripe_event";
    const QUEUE: &'static str = QUEUE;
}

/// The background job that tells Stripe how many seats an organization uses.
///
/// Queued by [`crate::billing::Limits::queue_seat_sync`] whenever a membership
/// changes, and only when some plan sells a per-seat price. The payload is the
/// organization and nothing else: the seat count is read when the job runs, so
/// a burst of invitations converges on one number rather than replaying every
/// intermediate one.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncSeats {
    /// The organization whose membership changed.
    pub organization_id: Uuid,
}

impl Job for SyncSeats {
    /// Namespaced, for the same reason [`ProcessStripeEvent`] is.
    const KIND: &'static str = "anubis.billing.sync_seats";
    const QUEUE: &'static str = QUEUE;
}

/// Writes the subscription projection from what Stripe says.
///
/// Register it on a worker and the framework owns the whole lifecycle:
///
/// ```ignore
/// let reconciler = anubis::billing::Reconciler::new(pool.clone(), plans.clone(), &config);
/// let worker = anubis::jobs::Worker::builder(pool)
///     .register(move |job: anubis::billing::ProcessStripeEvent| {
///         let reconciler = reconciler.clone();
///         async move { reconciler.process(job).await }
///     })
///     .build();
/// ```
///
/// Cloning is cheap: the pool, the plans, and the HTTP client are all handles.
#[derive(Clone)]
pub struct Reconciler {
    pool: DbPool,
    plans: Arc<PlanSet>,
    /// Absent until `STRIPE_SECRET_KEY` is set, which disables the lifecycle.
    stripe: Option<stripe::Client>,
}

impl Reconciler {
    /// Builds a reconciler over the application's plans and Stripe credentials.
    #[must_use]
    pub fn new(pool: DbPool, plans: PlanSet, config: &AppConfig) -> Self {
        Self {
            pool,
            plans: Arc::new(plans),
            stripe: config.stripe.as_ref().map(stripe::Client::new),
        }
    }

    /// Processes one stored event, stamping the outcome on its row.
    ///
    /// # Errors
    /// Returns the failure that was recorded, so the queue retries the job on
    /// its own backoff and eventually dead-letters it. An event that is already
    /// processed returns `Ok` without acting, which is what makes an
    /// at-least-once redelivery of the same job harmless.
    pub async fn process(&self, job: ProcessStripeEvent) -> Result<(), BoxError> {
        let mut connection = self.pool.get().await?;
        let record = StripeBillingEvent::find(&mut connection, job.event_id).await?;
        if record.processed_at.is_some() {
            return Ok(());
        }

        match self.act_on(&mut connection, &record).await {
            Ok(()) => {
                StripeBillingEvent::mark_processed(&mut connection, record.id).await?;
                Ok(())
            }
            Err(failure) => {
                // Recorded on the row as well as returned: the queue's own error
                // disappears when a retry succeeds, and the row is what an
                // operator reads when Stripe insists it sent something.
                StripeBillingEvent::mark_failed(&mut connection, record.id, failure.message())
                    .await?;
                Err(failure.into())
            }
        }
    }

    /// Bills an organization for the seats it is actually using.
    ///
    /// Runs [`SyncSeats`], and does nothing at all unless every condition for a
    /// per-seat charge holds: billing configured, a plan subscription that
    /// grants access, and a price its plan sells per seat. The quantity is set
    /// on the line item billing that price, which need not be the first one
    /// when the subscription carries add-ons, and Stripe's answer is written
    /// into the projection, so the row a screen renders agrees with the invoice
    /// the customer will get.
    ///
    /// Proration is Stripe's default, `create_prorations`: a seat added
    /// mid-period is charged for the part of the period it exists, and a seat
    /// removed earns a credit against the next invoice. That is the behavior
    /// customers expect from per-seat billing, and it is the account's own
    /// setting to change rather than this framework's.
    ///
    /// # Errors
    /// Returns the failure so the queue retries it. Running twice is harmless:
    /// a quantity already correct sends nothing.
    pub async fn sync_seats(&self, job: SyncSeats) -> Result<(), BoxError> {
        let Some(stripe) = self.stripe.as_ref() else {
            // Billing is off, so there is no subscription to bill against. Not
            // a failure: the membership change itself succeeded.
            return Ok(());
        };
        let mut connection = self.pool.get().await?;

        let Some(held) =
            Subscription::current_for_organization(&mut connection, job.organization_id)
                .await?
                .filter(Subscription::grants_access)
        else {
            return Ok(());
        };
        let Some(per_seat_price) = self.per_seat_price(&held) else {
            return Ok(());
        };

        // At least one: Stripe refuses a quantity of zero, and an organization
        // whose last member just left still holds the subscription they bought.
        let seats = limits::seats_used(&mut connection, job.organization_id)
            .await?
            .max(1);
        if seats == i64::from(held.quantity) {
            return Ok(());
        }

        // Every write below is Stripe's own answer, stamped when the read
        // began, exactly as reconciliation is.
        let origin = Origin {
            organization_id: Some(job.organization_id),
            metadata: None,
        };
        let read_at = unlocked_read_began();
        let current = stripe
            .retrieve_subscription(&held.stripe_subscription_id)
            .await?;
        let ended =
            SubscriptionStatus::parse(&current.status).is_some_and(SubscriptionStatus::is_terminal);
        let billed = current
            .item_for_price(per_seat_price)
            .and_then(stripe::SubscriptionItem::quantity);
        if ended || billed == Some(seats) {
            // Stripe already bills these seats, or bills nothing any more: an
            // earlier attempt whose answer was lost got there, or a
            // cancellation did. Sending the update again would only fetch that
            // attempt's saved answer, stale by now, so what Stripe holds is
            // written instead.
            self.converge(&mut connection, &current, &origin, read_at)
                .await?;
            return Ok(());
        }
        let item = current.item_for_price(per_seat_price).ok_or_else(|| {
            Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} has no line item billing {per_seat_price} to set \
                     a quantity on",
                    held.stripe_subscription_id,
                ),
            )
        })?;

        let update = SubscriptionUpdate {
            items: vec![ItemUpdate::SetQuantity {
                item_id: item.id(),
                quantity: seats,
            }],
            cancel_at_period_end: None,
        };
        // The row's last write is the revision: a retry of this job before
        // anything is written sends the same key, and the next membership
        // change, after this one's answer is recorded, sends a new one.
        let updated = match stripe
            .update_subscription(
                &held.stripe_subscription_id,
                &update,
                &held.updated_at.to_rfc3339(),
            )
            .await
        {
            Ok(updated) => updated,
            Err(refused) if refused.is_api() => {
                move_revision(&mut connection, held.id).await?;
                return Err(refused.into());
            }
            Err(unanswered) => return Err(unanswered.into()),
        };

        self.converge(&mut connection, &updated, &origin, read_at)
            .await?;

        let billed = updated
            .item_for_price(per_seat_price)
            .and_then(stripe::SubscriptionItem::quantity);
        if billed != Some(seats) {
            // A replayed answer to an earlier attempt, read back, and the
            // subscription moved since: these seats were never set. A fresh
            // revision lets the queue's retry send them as a new request.
            move_revision(&mut connection, held.id).await?;
            return Err(Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe answered an earlier seat update for {}, which no longer holds; the \
                     retry sends {seats} seats afresh",
                    held.stripe_subscription_id,
                ),
            )
            .into());
        }

        tracing::info!(
            organization.id = %job.organization_id,
            billing.subscription.id = updated.id,
            billing.seats.previous = held.quantity,
            billing.seats.current = seats,
            "seats on {{billing.subscription.id}} moved from {{billing.seats.previous}} to \
             {{billing.seats.current}}",
        );

        Ok(())
    }

    /// Charges a wallet's saved card for its armed recharge.
    ///
    /// Runs [`RechargeWallet`]; see [`crate::billing::wallet`] for when one is
    /// armed and how a lost answer is found again rather than charged twice.
    ///
    /// # Errors
    /// Returns the failure so the queue retries it when Stripe's answer is
    /// unknown. A declined card is not a failure: it ends the attempt and turns
    /// auto-recharge off.
    pub async fn recharge_wallet(&self, job: RechargeWallet) -> Result<(), BoxError> {
        let Some(stripe) = self.stripe.as_ref() else {
            // Billing is off, so there is no card to charge. The attempt stays
            // armed and runs again once a spend finds it stale.
            return Ok(());
        };
        payments::recharge(&self.pool, &self.plans, stripe, job.organization_id).await?;
        Ok(())
    }

    /// The Stripe price the plan subscription bills per seat, if it does.
    ///
    /// Read from the plan the row names rather than from Stripe, because that
    /// is the same file the checkout bought from and the events keep current.
    fn per_seat_price(&self, held: &Subscription) -> Option<&str> {
        self.plans
            .find(held.plan_key.as_deref()?)
            .zip(Interval::parse(&held.billing_interval))
            .and_then(|(plan, interval)| plan.price(interval))
            .filter(|price| price.is_per_seat())
            .map(super::plans::Price::stripe_price_id)
    }

    /// Converges an organization's subscriptions with Stripe's own record.
    ///
    /// This is the repair path for an event that was never delivered, never
    /// processed, or processed against a plan the file did not define yet. It
    /// reads every subscription Stripe holds for the organization's customer,
    /// following Stripe's pages to the end, and writes each one with its line
    /// items, so a row that drifted is corrected and a row that should exist
    /// is created.
    ///
    /// Each subscription converges in its own transaction. One that cannot be
    /// recorded (a price `config/billing.yml` does not sell, a second live
    /// subscription for one subject) leaves its own row as it was and is named
    /// in the error, and every other subscription is still repaired: an
    /// organization paying for a hundred records is not held hostage by one.
    ///
    /// Returns the organization's live subscriptions afterwards, plan
    /// subscription first. An organization that never reached Stripe has
    /// nothing to converge and answers with what it already had.
    ///
    /// # Errors
    /// Returns an [`Error`] when billing is not configured, when Stripe refuses
    /// or cannot be reached, when a database write fails, or when any
    /// subscription could not be recorded.
    pub async fn reconcile(&self, organization_id: Uuid) -> Result<Vec<Subscription>, Error> {
        let stripe = self.stripe.as_ref().ok_or_else(Error::unconfigured)?;
        let mut connection = self.pool.get().await.map_err(|source| {
            Error::new(
                ErrorKind::Database,
                format!("no database connection: {source}"),
            )
        })?;

        let Some(customer_id) = customer_of(&mut connection, organization_id).await? else {
            // No customer means no purchase was ever started, so Stripe holds
            // nothing for this organization and there is nothing to correct.
            return Ok(
                Subscription::live_for_organization(&mut connection, organization_id).await?,
            );
        };

        let read_at = unlocked_read_began();
        let mut held = stripe.list_subscriptions(&customer_id).await?;
        // Finished subscriptions first: a row this application still believes is
        // live has to be corrected before the live one can take its place, or
        // the one-live-subscription indexes refuse the insert.
        held.sort_by_key(|subscription| !is_terminal(&subscription.status));

        let organization = Origin {
            organization_id: Some(organization_id),
            metadata: None,
        };
        let mut unrecorded = Vec::new();
        for subscription in &held {
            if let Err(failure) = self
                .converge(&mut connection, subscription, &organization, read_at)
                .await
            {
                tracing::warn!(
                    organization.id = %organization_id,
                    billing.subscription.id = subscription.id,
                    error.message = failure.message(),
                    "reconciliation could not record {{billing.subscription.id}}: \
                     {{error.message}}",
                );
                unrecorded.push(format!("{}: {}", subscription.id, failure.message()));
            }
        }

        if self.plans.wallet().is_some() {
            // The wallet's payments converge the same way: a top-up whose
            // event was missed is credited, and a refund debited.
            for payment in stripe.list_payment_intents(&customer_id, None).await? {
                if let Err(failure) = payments::converge_payment(
                    &mut connection,
                    &self.plans,
                    stripe,
                    &payment,
                    Refunds::WhenRefunded,
                )
                .await
                {
                    unrecorded.push(format!("{}: {}", payment.id, failure.message()));
                }
            }
        }

        tracing::info!(
            organization.id = %organization_id,
            billing.subscriptions.read = held.len(),
            billing.subscriptions.unrecorded = unrecorded.len(),
            "reconciled {{billing.subscriptions.read}} Stripe subscriptions for \
             organization {{organization.id}}",
        );
        if !unrecorded.is_empty() {
            return Err(Error::new(
                ErrorKind::Data,
                format!(
                    "{} Stripe subscriptions or payments could not be recorded; the rest were \
                     repaired. {}",
                    unrecorded.len(),
                    unrecorded.join("; "),
                ),
            ));
        }

        Ok(Subscription::live_for_organization(&mut connection, organization_id).await?)
    }

    /// Acts on one stored event, whatever kind it turns out to be.
    async fn act_on(
        &self,
        connection: &mut AsyncPgConnection,
        record: &StripeBillingEvent,
    ) -> Result<(), Error> {
        let stripe = self.stripe.as_ref().ok_or_else(Error::unconfigured)?;
        let Some(object) = record.object() else {
            return Err(Error::new(
                ErrorKind::Data,
                format!("event {} carries no data.object", record.stripe_event_id),
            ));
        };
        // The event's own creation time is the ordering key. A document without
        // one falls back to when it arrived, which is close enough and keeps a
        // malformed timestamp from stopping the lifecycle.
        let event_at = record.stripe_created_at.unwrap_or(record.received_at);

        match record.event_type.as_str() {
            CHECKOUT_COMPLETED => {
                let session = CompletedCheckout::deserialize(object).map_err(|source| {
                    Error::new(
                        ErrorKind::Data,
                        format!("the completed checkout session could not be read: {source}"),
                    )
                })?;
                let Some(subscription_id) = session.subscription.as_deref() else {
                    // A one-off payment rather than a subscription: a top-up
                    // when the wallet's metadata names it, and otherwise a fact
                    // about Stripe rather than about this application.
                    return self
                        .converge_payment_named(
                            connection,
                            session.payment_intent.as_deref(),
                            Refunds::Skip,
                        )
                        .await;
                };

                let subscription = stripe.retrieve_subscription(subscription_id).await?;
                let origin = Origin {
                    organization_id: None,
                    // The session carries the subscription's organization, and
                    // is the fallback for resolving it the day Stripe drops it.
                    metadata: Some(&session.metadata),
                };
                self.converge(connection, &subscription, &origin, event_at)
                    .await
            }
            SUBSCRIPTION_CREATED | SUBSCRIPTION_UPDATED => {
                let announced = read_subscription(object)?;
                // Read back rather than trusted: the body is a snapshot of when
                // the event was created, and this job may be minutes behind it.
                let current = stripe.retrieve_subscription(&announced.id).await?;
                self.converge(connection, &current, &Origin::default(), event_at)
                    .await
            }
            SUBSCRIPTION_DELETED => {
                // Trusted as it arrives: a cancelled subscription is over, and
                // nothing Stripe could say about it later would be newer. Only a
                // long item list is read further, which costs nothing otherwise.
                let ended = stripe.complete_items(read_subscription(object)?).await?;
                self.converge(connection, &ended, &Origin::default(), event_at)
                    .await
            }
            CHECKOUT_ASYNC_PAYMENT_SUCCEEDED => {
                let session = CompletedCheckout::deserialize(object).map_err(|source| {
                    Error::data(format!("the checkout session could not be read: {source}"))
                })?;
                self.converge_payment_named(
                    connection,
                    session.payment_intent.as_deref(),
                    Refunds::Skip,
                )
                .await
            }
            PAYMENT_SUCCEEDED | PAYMENT_FAILED => {
                // Every payment the account takes raises these, an invoice's
                // included. The body's metadata decides only whether the
                // payment is a wallet's, which saves reading back every other
                // one; what it says about money is read back from Stripe.
                if object
                    .get("metadata")
                    .and_then(|metadata| metadata.get(stripe::WALLET_METADATA))
                    .is_none()
                {
                    return Ok(());
                }
                let named = object.get("id").and_then(serde_json::Value::as_str);
                self.converge_payment_named(connection, named, Refunds::Skip)
                    .await
            }
            CHARGE_REFUNDED
            | CHARGE_REFUND_UPDATED
            | REFUND_CREATED
            | REFUND_UPDATED
            | REFUND_FAILED => {
                // A charge and a refund both name the payment they belong to,
                // and every refund of it is read back, so which event this is,
                // and in what order they arrive, does not matter.
                let named = object
                    .get("payment_intent")
                    .and_then(serde_json::Value::as_str);
                self.converge_payment_named(connection, named, Refunds::Read)
                    .await
            }
            other => {
                // Stored, stamped, and ignored. A receiver that failed on the
                // types it does not know would fail on every feature Stripe
                // ships, and an endpoint subscribed to "all events" is the
                // normal way people configure one.
                tracing::debug!(
                    billing.event.type = other,
                    billing.event.id = record.stripe_event_id,
                    "ignoring Stripe event {{billing.event.id}} of type {{billing.event.type}}",
                );
                Ok(())
            }
        }
    }

    /// Reads a payment an event named back from Stripe, and converges it.
    ///
    /// An event naming no payment is a fact about Stripe rather than about
    /// this application, and so is a payment that is not a wallet's.
    async fn converge_payment_named(
        &self,
        connection: &mut AsyncPgConnection,
        payment_intent_id: Option<&str>,
        refunds: Refunds,
    ) -> Result<(), Error> {
        let stripe = self.stripe.as_ref().ok_or_else(Error::unconfigured)?;
        let Some(payment_intent_id) = payment_intent_id else {
            tracing::debug!("a Stripe event named no payment, so the wallet has nothing to record");
            return Ok(());
        };
        let payment = stripe.retrieve_payment_intent(payment_intent_id).await?;
        payments::converge_payment(connection, &self.plans, stripe, &payment, refunds).await
    }

    /// Writes one Stripe subscription, and its line items, into the projection.
    ///
    /// Both in one transaction behind the ordering guard: when the guard keeps
    /// the subscription as it is, the items are left alone too, and when it
    /// applies, the items become exactly the ones Stripe reported.
    pub(super) async fn converge(
        &self,
        connection: &mut AsyncPgConnection,
        subscription: &stripe::Subscription,
        origin: &Origin<'_>,
        event_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        // The `WHERE` of an `ON CONFLICT DO UPDATE` is this trait's method, and
        // the prelude's `QueryDsl::filter` does not reach an insert statement.
        // Imported here rather than at the top of the module because there it
        // would be ambiguous with every ordinary `table.filter(..)`.
        use diesel::query_dsl::methods::FilterDsl as _;

        if !subscription.items_complete() {
            // Deleting the items Stripe did not list is only right when the
            // list is whole, and the client completes one before handing it
            // over, so this is a page that could not be completed.
            return Err(Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} arrived with only part of its item list",
                    subscription.id,
                ),
            ));
        }

        let organization_id = self
            .resolve_organization(connection, subscription, origin)
            .await?;
        let subject = read_subject(subscription)?;
        let bought = match &subject {
            None => self.resolve_plan(subscription)?,
            Some(_subject) => Bought::subject(subscription)?,
        };

        if SubscriptionStatus::parse(&subscription.status).is_none() {
            // Stored verbatim anyway: an unfamiliar word grants no access and
            // is not terminal, which is the safe reading, and the log is what
            // tells an operator this framework version is behind Stripe.
            tracing::warn!(
                billing.subscription.id = subscription.id,
                billing.subscription.status = subscription.status,
                "Stripe reports status {{billing.subscription.status}}, which this version \
                 does not know",
            );
        }

        let quantity = i32::try_from(bought.quantity).unwrap_or(i32::MAX);
        let period_end = subscription
            .period_end()
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
        let values = (
            subscriptions::organization_id.eq(organization_id),
            subscriptions::plan_key.eq(bought.plan_key),
            subscriptions::subject_type.eq(subject.as_ref().map(Subject::kind)),
            subscriptions::subject_id.eq(subject.as_ref().map(Subject::id)),
            subscriptions::stripe_subscription_id.eq(&subscription.id),
            subscriptions::status.eq(&subscription.status),
            subscriptions::billing_interval.eq(bought.interval.as_str()),
            subscriptions::quantity.eq(quantity),
            subscriptions::current_period_end.eq(period_end),
            subscriptions::cancel_at_period_end.eq(subscription.cancel_at_period_end),
            subscriptions::stripe_event_at.eq(event_at),
        );

        let written = connection
            .transaction::<_, Error, _>(async |connection| {
                let written: Option<Uuid> = diesel::insert_into(subscriptions::table)
                    .values(values)
                    .on_conflict(subscriptions::stripe_subscription_id)
                    .do_update()
                    .set(values)
                    // The ordering guard: an event older than the one already
                    // applied changes nothing. See the module docs.
                    .filter(subscriptions::stripe_event_at.is_null().or(
                        subscriptions::stripe_event_at.le(excluded(subscriptions::stripe_event_at)),
                    ))
                    // And a subscription that is over stays over: Stripe never
                    // revives a cancelled or expired one, so a live status
                    // arriving after a terminal one is an older read, whatever
                    // its stamp says.
                    .filter(
                        subscriptions::status
                            .ne_all(terminal_statuses())
                            .or(excluded(subscriptions::status).eq_any(terminal_statuses())),
                    )
                    .returning(subscriptions::id)
                    .get_result(connection)
                    .await
                    .optional()
                    .map_err(|source| write_failed(subscription, organization_id, source))?;

                if let Some(row_id) = written {
                    write_items(connection, row_id, subscription).await?;
                }
                Ok(written)
            })
            .await?;

        if written.is_none() {
            tracing::info!(
                billing.subscription.id = subscription.id,
                organization.id = %organization_id,
                "a Stripe read older than the one already applied, or one reviving a \
                 subscription that is over, left {{billing.subscription.id}} alone",
            );
        } else {
            tracing::info!(
                billing.subscription.id = subscription.id,
                billing.subscription.status = subscription.status,
                organization.id = %organization_id,
                "subscription {{billing.subscription.id}} is {{billing.subscription.status}} \
                 for organization {{organization.id}}",
            );
        }

        Ok(())
    }

    /// The organization a Stripe subscription belongs to.
    ///
    /// Three answers, in order of how much they can be trusted: one the caller
    /// resolved already, the metadata the checkout put on the subscription, and
    /// the customer this application stored when that checkout began.
    async fn resolve_organization(
        &self,
        connection: &mut AsyncPgConnection,
        subscription: &stripe::Subscription,
        origin: &Origin<'_>,
    ) -> Result<Uuid, Error> {
        if let Some(known) = origin.organization_id {
            return Ok(known);
        }

        let announced = subscription
            .metadata(ORGANIZATION_METADATA)
            .or_else(|| {
                origin
                    .metadata
                    .and_then(|metadata| metadata.get(ORGANIZATION_METADATA))
                    .map(String::as_str)
            })
            .and_then(|value| value.parse::<Uuid>().ok());

        if let Some(organization_id) = announced
            && organization_exists(connection, organization_id).await?
        {
            return Ok(organization_id);
        }

        // A subscription created in the Stripe dashboard carries no metadata,
        // and the customer is the mapping this application wrote itself.
        organization_of(connection, &subscription.customer)
            .await?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Data,
                    format!(
                        "Stripe subscription {} belongs to customer {}, which names no \
                         organization here; its metadata names none either",
                        subscription.id, subscription.customer,
                    ),
                )
            })
    }

    /// What a plan subscription buys: its plan, its interval, and its seats.
    ///
    /// The price is the authority, because changing plan in the customer portal
    /// changes the price and leaves the metadata saying what was bought first.
    /// Any of the subscription's items may carry it, since a plan subscription
    /// can carry add-ons beside the plan, but only one plan's price may appear.
    fn resolve_plan(&self, subscription: &stripe::Subscription) -> Result<Bought<'_>, Error> {
        let mut found: Option<Bought<'_>> = None;
        for item in subscription.items() {
            let Some((plan, interval)) = item
                .price_id()
                .and_then(|price_id| self.plans.find_by_price_id(price_id))
            else {
                continue;
            };
            if let Some(earlier) = found.as_ref().and_then(|bought| bought.plan_key)
                && earlier != plan.key()
            {
                return Err(Error::new(
                    ErrorKind::Data,
                    format!(
                        "Stripe subscription {} carries the prices of two plans, {earlier} and \
                         {}; a plan subscription buys one",
                        subscription.id,
                        plan.key(),
                    ),
                ));
            }
            found = Some(Bought {
                plan_key: Some(plan.key()),
                interval,
                quantity: item.quantity().unwrap_or(1),
            });
        }
        if let Some(bought) = found {
            return Ok(bought);
        }

        let Some(first) = subscription.items().next() else {
            return Err(Error::new(
                ErrorKind::Data,
                format!("Stripe subscription {} has no price", subscription.id),
            ));
        };
        let price_id = first.price_id().unwrap_or("(none)");

        // No price is one `config/billing.yml` sells. The subscription may
        // still name a plan this application knows, which is what a price
        // replaced in Stripe but not here looks like.
        let announced = subscription
            .metadata(PLAN_METADATA)
            .and_then(|key| self.plans.find(key));
        let interval = subscription.recurring_interval().and_then(Interval::parse);

        match (announced, interval) {
            (Some(plan), Some(interval)) => {
                tracing::warn!(
                    billing.price = price_id,
                    billing.plan = plan.key(),
                    "Stripe price {{billing.price}} is not in config/billing.yml; falling back \
                     to the plan the subscription's metadata names, {{billing.plan}}",
                );
                Ok(Bought {
                    plan_key: Some(plan.key()),
                    interval,
                    quantity: first.quantity().unwrap_or(1),
                })
            }
            _unknown => Err(Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} is on price {price_id}, which config/billing.yml \
                     does not sell; add the price to the file, or stop offering the product in \
                     the Stripe customer portal",
                    subscription.id,
                ),
            )),
        }
    }
}

/// What a subscription buys, in the columns the projection stores it in.
#[derive(Debug)]
struct Bought<'plans> {
    /// The plan, for a plan subscription; `None` for a subject subscription.
    plan_key: Option<&'plans str>,
    /// How often it bills.
    interval: Interval,
    /// Seats bought: the plan item's quantity, and one for a subject.
    quantity: i64,
}

impl Bought<'_> {
    /// What a subject subscription buys: no plan, on its prices' interval.
    ///
    /// Its prices are the application's own rather than `config/billing.yml`'s,
    /// so nothing here looks them up; only the recurrence has to be one the
    /// framework records.
    fn subject(subscription: &stripe::Subscription) -> Result<Self, Error> {
        let recurrence = subscription.recurring_interval();
        let interval = recurrence.and_then(Interval::parse).ok_or_else(|| {
            Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} recurs every {}, and subscriptions are recorded \
                     monthly or yearly",
                    subscription.id,
                    recurrence.unwrap_or("(no interval)"),
                ),
            )
        })?;

        Ok(Self {
            plan_key: None,
            interval,
            quantity: 1,
        })
    }
}

/// The record a Stripe subscription pays for, from the metadata checkout put on it.
///
/// Both keys or neither: a subscription naming half a subject is a mistake
/// somebody made in the Stripe dashboard, and recording it as the plan
/// subscription would be a second one.
fn read_subject(subscription: &stripe::Subscription) -> Result<Option<Subject>, Error> {
    let kind = subscription
        .metadata(SUBJECT_TYPE_METADATA)
        .filter(|value| !value.is_empty());
    let id = subscription
        .metadata(SUBJECT_ID_METADATA)
        .filter(|value| !value.is_empty());

    match (kind, id) {
        (None, None) => Ok(None),
        (Some(kind), Some(id)) => id
            .parse::<Uuid>()
            .map(|id| Some(Subject::new(kind, id)))
            .map_err(|source| {
                Error::new(
                    ErrorKind::Data,
                    format!(
                        "Stripe subscription {} names subject id {id:?}, which is not a UUID: \
                         {source}",
                        subscription.id,
                    ),
                )
            }),
        _half => Err(Error::new(
            ErrorKind::Data,
            format!(
                "Stripe subscription {} names half a subject in its metadata; set both \
                 {SUBJECT_TYPE_METADATA} and {SUBJECT_ID_METADATA}, or neither",
                subscription.id,
            ),
        )),
    }
}

/// Makes a subscription's stored line items exactly the ones Stripe reported.
///
/// Runs inside the transaction that wrote the subscription, after its ordering
/// guard applied: an item Stripe no longer lists is deleted, and every listed
/// one is inserted or brought up to date by its Stripe id.
async fn write_items(
    connection: &mut AsyncPgConnection,
    subscription_id: Uuid,
    subscription: &stripe::Subscription,
) -> Result<(), Error> {
    let reported: Vec<&str> = subscription
        .items()
        .map(stripe::SubscriptionItem::id)
        .collect();
    diesel::delete(
        subscription_items::table
            .filter(subscription_items::subscription_id.eq(subscription_id))
            .filter(subscription_items::stripe_item_id.ne_all(&reported)),
    )
    .execute(connection)
    .await?;

    for item in subscription.items() {
        let Some(price_id) = item.price_id().filter(|_price| !item.id().is_empty()) else {
            return Err(Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} lists a line item without an id or a price",
                    subscription.id,
                ),
            ));
        };
        let quantity = item
            .quantity()
            .map(|quantity| i32::try_from(quantity).unwrap_or(i32::MAX));
        let values = (
            subscription_items::subscription_id.eq(subscription_id),
            subscription_items::stripe_item_id.eq(item.id()),
            subscription_items::stripe_price_id.eq(price_id),
            subscription_items::quantity.eq(quantity),
        );

        diesel::insert_into(subscription_items::table)
            .values(values)
            .on_conflict(subscription_items::stripe_item_id)
            .do_update()
            .set(values)
            .execute(connection)
            .await?;
    }

    Ok(())
}

/// Moves a subscription row's revision after Stripe refused a write to it.
///
/// A write's idempotency key is derived from the row's last write, and Stripe
/// saves the first answer to a key, refusals and `500`s included, replaying it
/// to every retry for a day. Without this a refused write would be refused
/// again on each retry until something else happened to write the row. A
/// fresh key is safe because every write reads Stripe back first: a change
/// that did apply is found there and not sent again.
///
/// # Errors
/// Returns the underlying Diesel error when the update fails.
pub(super) async fn move_revision(
    connection: &mut AsyncPgConnection,
    subscription_id: Uuid,
) -> QueryResult<()> {
    diesel::update(subscriptions::table.find(subscription_id))
        .set(subscriptions::updated_at.eq(Utc::now()))
        .execute(connection)
        .await?;
    Ok(())
}

/// The stamp for a write that read Stripe directly, rather than for an event.
///
/// Two rules in one value. It is when the read *began*, not when the write
/// lands: anything Stripe recorded after that moment may be missing from what
/// was read, so an event created then must stay free to apply, and letting an
/// event win is always safe because processing one reads Stripe back. And it
/// is in whole seconds, because an event's `created` is: a cancellation created
/// later in the same second would otherwise compare as older than this write
/// and be dropped, leaving a cancelled subscription granting access.
pub(super) fn read_began() -> DateTime<Utc> {
    let now = Utc::now();
    DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now)
}

/// The stamp for a write that read Stripe without holding the subscription's row:
/// one second before [`read_began`], so it loses every tie.
///
/// Reconciliation and seat synchronization read and write without the lock a
/// change holds, so an event created in the second their read began can be
/// processed before their write lands. An event written first carries that
/// same second, and a tie would let the older read overwrite it, reviving a
/// cancellation that nothing would repair. Losing ties to events is always
/// safe: processing an event reads Stripe back.
pub(super) fn unlocked_read_began() -> DateTime<Utc> {
    read_began() - TimeDelta::seconds(1)
}

/// Explains a refused write in the terms of what it was trying to do.
fn write_failed(
    subscription: &stripe::Subscription,
    organization_id: Uuid,
    source: diesel::result::Error,
) -> Error {
    if matches!(
        source,
        diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::UniqueViolation, _)
    ) {
        // The unique indexes this write can trip are the two live-subscription
        // ones, and hitting either means Stripe holds two subscriptions for
        // one slot. A person has to decide which one the customer keeps.
        // The plan slot is one organization's; a record's slot spans every
        // organization, so the subscription holding it may be another's.
        let holder = read_subject(subscription).ok().flatten().map_or_else(
            || format!("organization {organization_id} already holds a live plan subscription"),
            |subject| format!("{subject} already has a live subscription"),
        );
        return Error::new(
            ErrorKind::Data,
            format!(
                "{holder}, so Stripe subscription {} for organization {organization_id} could not \
                 be recorded; cancel one of them at Stripe",
                subscription.id,
            ),
        );
    }

    Error::from(source)
}

impl fmt::Debug for Reconciler {
    /// Written by hand rather than derived: the connection pool is not `Debug`,
    /// and the Stripe client must never render its key anyway.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reconciler")
            .field("plans", &self.plans.plans().len())
            .field("stripe", &self.stripe.is_some())
            .finish_non_exhaustive()
    }
}

/// What a caller already knows about who a Stripe subscription belongs to.
#[derive(Debug, Default)]
pub(super) struct Origin<'a> {
    /// The organization, when the caller resolved it already.
    pub(super) organization_id: Option<Uuid>,
    /// A checkout session's metadata, when the subscription carries none.
    pub(super) metadata: Option<&'a BTreeMap<String, String>>,
}

/// Reads a subscription out of an event's `data.object`.
fn read_subscription(object: &serde_json::Value) -> Result<stripe::Subscription, Error> {
    stripe::Subscription::deserialize(object).map_err(|source| {
        Error::new(
            ErrorKind::Data,
            format!("the event's subscription could not be read: {source}"),
        )
    })
}

/// Whether a status word means the subscription is over.
///
/// A word this version does not know is not terminal, which keeps an unfamiliar
/// subscription occupying its live slot rather than letting a second one start
/// beside it.
fn is_terminal(status: &str) -> bool {
    SubscriptionStatus::parse(status).is_some_and(SubscriptionStatus::is_terminal)
}

/// The Stripe customer stored on an organization, if it has one.
async fn customer_of(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<Option<String>, Error> {
    Ok(organizations::table
        .find(organization_id)
        .select(organizations::stripe_customer_id)
        .first::<Option<String>>(connection)
        .await
        .optional()?
        .flatten())
}

/// The organization a Stripe customer is, if this application knows it.
async fn organization_of(
    connection: &mut AsyncPgConnection,
    customer_id: &str,
) -> Result<Option<Uuid>, Error> {
    Ok(organizations::table
        .filter(organizations::stripe_customer_id.eq(customer_id))
        .select(organizations::id)
        .first(connection)
        .await
        .optional()?)
}

/// Whether an organization named in Stripe's metadata still exists here.
async fn organization_exists(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<bool, Error> {
    Ok(organizations::table
        .find(organization_id)
        .select(organizations::id)
        .first::<Uuid>(connection)
        .await
        .optional()?
        .is_some())
}

/// What went wrong keeping the projection in line with Stripe.
#[derive(Debug)]
enum ErrorKind {
    /// Billing is not configured, so Stripe cannot be read at all.
    Unconfigured,
    /// Stripe refused the call or could not be reached.
    Stripe {
        /// Whether the request never got an answer, which is worth retrying.
        transport: bool,
    },
    /// The database refused the write.
    Database,
    /// Stripe said something this application cannot act on.
    Data,
}

/// A subscription that could not be brought in line with Stripe.
///
/// The message is written for whoever has to fix it, and names the Stripe
/// object and the configuration involved. It never carries a credential: the
/// Stripe key travels in a header and no part of a header reaches here.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// Returns `true` when Stripe could not be reached, so retrying may work.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Stripe { transport: true })
    }

    /// Returns `true` when this deployment has no Stripe credentials.
    #[must_use]
    pub fn is_unconfigured(&self) -> bool {
        matches!(self.kind, ErrorKind::Unconfigured)
    }

    /// The failure on its own, without the backtrace [`Display`] adds.
    ///
    /// This is what is stamped on the event row, where a backtrace would be
    /// noise rather than evidence.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            backtrace: Backtrace::capture(),
        }
    }

    fn unconfigured() -> Self {
        Self::new(
            ErrorKind::Unconfigured,
            "billing is not configured for this deployment: set STRIPE_SECRET_KEY",
        )
    }

    /// Stripe said something this application cannot act on.
    pub(super) fn data(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Data, message)
    }
}

impl From<super::wallet::Error> for Error {
    fn from(source: super::wallet::Error) -> Self {
        Self::new(ErrorKind::Data, source.message())
    }
}

impl From<stripe::Error> for Error {
    fn from(source: stripe::Error) -> Self {
        Self::new(
            ErrorKind::Stripe {
                transport: source.is_transport(),
            },
            source.to_string(),
        )
    }
}

impl From<diesel::result::Error> for Error {
    fn from(source: diesel::result::Error) -> Self {
        Self::new(ErrorKind::Database, format!("the write failed: {source}"))
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::{HANDLED_EVENTS, ProcessStripeEvent, QUEUE, is_terminal};
    use crate::jobs::Job;

    #[test]
    fn the_job_is_namespaced_and_runs_on_its_own_queue() {
        assert!(ProcessStripeEvent::KIND.starts_with("anubis."));
        assert_eq!(ProcessStripeEvent::QUEUE, QUEUE);
        assert_ne!(ProcessStripeEvent::QUEUE, crate::jobs::DEFAULT_QUEUE);
        assert_ne!(
            ProcessStripeEvent::QUEUE,
            crate::webhooks::QUEUE,
            "outgoing deliveries must not be able to starve billing",
        );
    }

    #[test]
    fn the_handled_events_are_the_ones_an_endpoint_subscribes() {
        assert!(HANDLED_EVENTS.contains(&"checkout.session.completed"));
        assert!(HANDLED_EVENTS.contains(&"customer.subscription.deleted"));
        for event in HANDLED_EVENTS {
            assert!(
                event.contains('.') && event.to_lowercase() == event,
                "{event} is not spelled the way Stripe spells an event type",
            );
        }
    }

    #[test]
    fn an_unfamiliar_status_holds_its_organizations_live_slot() {
        assert!(is_terminal("canceled"));
        assert!(is_terminal("incomplete_expired"));
        assert!(!is_terminal("active"));
        assert!(
            !is_terminal("a_status_from_2030"),
            "an unknown word must not free the slot a subscription is occupying",
        );
    }
}
