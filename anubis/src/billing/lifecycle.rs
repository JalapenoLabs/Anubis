//! Keeping the subscription projection in line with Stripe.
//!
//! Money lives at Stripe; the `subscriptions` table is the projection this
//! application authorizes and renders from. [`Reconciler`] is the only thing
//! that writes it, and it has two ways in:
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
//! Reconciliation stamps `now`, because it read Stripe directly and is
//! therefore at least as current as any event in flight.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::upsert::excluded;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::StripeBillingEvent;
use super::limits;
use super::model::{Subscription, SubscriptionStatus};
use super::plans::{Interval, PlanSet};
use super::stripe::{self, CompletedCheckout};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::jobs::{BoxError, Job};
use crate::schema::{organizations, subscriptions};

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
pub const HANDLED_EVENTS: [&str; 4] = [
    CHECKOUT_COMPLETED,
    SUBSCRIPTION_CREATED,
    SUBSCRIPTION_UPDATED,
    SUBSCRIPTION_DELETED,
];

/// A purchase finished at Stripe Checkout; the subscription now exists.
const CHECKOUT_COMPLETED: &str = "checkout.session.completed";

/// A subscription was created, by checkout or in the Stripe dashboard.
const SUBSCRIPTION_CREATED: &str = "customer.subscription.created";

/// A subscription changed: its plan, its status, its quantity, its period.
const SUBSCRIPTION_UPDATED: &str = "customer.subscription.updated";

/// A subscription ended, and nothing further will be charged for it.
const SUBSCRIPTION_DELETED: &str = "customer.subscription.deleted";

/// Stripe's metadata key naming the organization a subscription belongs to.
const ORGANIZATION_METADATA: &str = "organization_id";

/// Stripe's metadata key naming the plan a checkout bought.
const PLAN_METADATA: &str = "plan_key";

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
    /// per-seat charge holds: billing configured, a subscription that grants
    /// access, and a price its plan sells per seat. The quantity is read back
    /// from Stripe and written into the projection, so the row a screen renders
    /// agrees with the invoice the customer will get.
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
        if !self.charges_per_seat(&held) {
            return Ok(());
        }

        // At least one: Stripe refuses a quantity of zero, and an organization
        // whose last member just left still holds the subscription they bought.
        let seats = limits::seats_used(&mut connection, job.organization_id)
            .await?
            .max(1);
        if seats == i64::from(held.quantity) {
            return Ok(());
        }

        let current = stripe
            .retrieve_subscription(&held.stripe_subscription_id)
            .await?;
        let item_id = current.item_id().ok_or_else(|| {
            Error::new(
                ErrorKind::Data,
                format!(
                    "Stripe subscription {} has no line item to set a quantity on",
                    held.stripe_subscription_id,
                ),
            )
        })?;

        let updated = stripe
            .update_subscription_quantity(&held.stripe_subscription_id, item_id, seats)
            .await?;

        tracing::info!(
            organization.id = %job.organization_id,
            billing.subscription.id = updated.id,
            billing.seats.previous = held.quantity,
            billing.seats.current = seats,
            "seats on {{billing.subscription.id}} moved from {{billing.seats.previous}} to \
             {{billing.seats.current}}",
        );

        // Written from Stripe's answer, stamped now, exactly as reconciliation
        // is: this read Stripe directly and is at least as current as anything
        // in flight.
        let origin = Origin {
            organization_id: Some(job.organization_id),
            metadata: None,
        };
        self.converge(&mut connection, &updated, &origin, Utc::now())
            .await?;

        Ok(())
    }

    /// Whether the subscription's own price is charged per seat.
    ///
    /// Read from the plan the row names rather than from Stripe, because that
    /// is the same file the checkout bought from and the events keep current.
    fn charges_per_seat(&self, held: &Subscription) -> bool {
        self.plans
            .find(&held.plan_key)
            .zip(Interval::parse(&held.billing_interval))
            .and_then(|(plan, interval)| plan.price(interval))
            .is_some_and(super::plans::Price::is_per_seat)
    }

    /// Converges an organization's subscriptions with Stripe's own record.
    ///
    /// This is the repair path for an event that was never delivered, never
    /// processed, or processed against a plan the file did not define yet. It
    /// reads every subscription Stripe holds for the organization's customer
    /// and writes each one, so a row that drifted is corrected and a row that
    /// should exist is created.
    ///
    /// Returns the organization's live subscription afterwards, which is `None`
    /// when it is on the free plan. An organization that never reached Stripe
    /// has nothing to converge and answers with what it already had.
    ///
    /// # Errors
    /// Returns an [`Error`] when billing is not configured, when Stripe refuses
    /// or cannot be reached, or when the database write fails.
    pub async fn reconcile(&self, organization_id: Uuid) -> Result<Option<Subscription>, Error> {
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
                Subscription::current_for_organization(&mut connection, organization_id).await?,
            );
        };

        let mut held = stripe.list_subscriptions(&customer_id).await?;
        // Finished subscriptions first: a row this application still believes is
        // live has to be corrected before the live one can take its place, or
        // the one-live-subscription index refuses the insert.
        held.sort_by_key(|subscription| !is_terminal(&subscription.status));

        let read_at = Utc::now();
        let organization = Origin {
            organization_id: Some(organization_id),
            metadata: None,
        };
        // One transaction for the whole customer, so a subscription that cannot
        // be recorded leaves the projection as it was rather than half moved.
        connection
            .transaction::<_, Error, _>(async |connection| {
                for subscription in &held {
                    self.converge(connection, subscription, &organization, read_at)
                        .await?;
                }
                Ok(())
            })
            .await?;

        tracing::info!(
            organization.id = %organization_id,
            billing.subscriptions.read = held.len(),
            "reconciled {{billing.subscriptions.read}} Stripe subscriptions for \
             organization {{organization.id}}",
        );

        Ok(Subscription::current_for_organization(&mut connection, organization_id).await?)
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
                    // A one-off payment rather than a subscription. Nothing here
                    // bills for those, so the event is a fact about Stripe and
                    // not about this application.
                    tracing::debug!(
                        billing.checkout.session = session.id,
                        "checkout session {{billing.checkout.session}} bought no subscription",
                    );
                    return Ok(());
                };

                let subscription = stripe.retrieve_subscription(subscription_id).await?;
                let origin = Origin {
                    organization_id: None,
                    // The session carries the same two values as the
                    // subscription, and is the fallback for the day Stripe
                    // drops one of them.
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
                // nothing Stripe could say about it later would be newer.
                let ended = read_subscription(object)?;
                self.converge(connection, &ended, &Origin::default(), event_at)
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

    /// Writes one Stripe subscription into the projection.
    async fn converge(
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

        let organization_id = self
            .resolve_organization(connection, subscription, origin)
            .await?;
        let (plan_key, interval) = self.resolve_plan(subscription)?;

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

        let quantity = i32::try_from(subscription.quantity()).unwrap_or(i32::MAX);
        let period_end = subscription
            .period_end()
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
        let values = (
            subscriptions::organization_id.eq(organization_id),
            subscriptions::plan_key.eq(plan_key),
            subscriptions::stripe_subscription_id.eq(&subscription.id),
            subscriptions::status.eq(&subscription.status),
            subscriptions::billing_interval.eq(interval.as_str()),
            subscriptions::quantity.eq(quantity),
            subscriptions::current_period_end.eq(period_end),
            subscriptions::cancel_at_period_end.eq(subscription.cancel_at_period_end),
            subscriptions::stripe_event_at.eq(event_at),
        );

        let written =
            diesel::insert_into(subscriptions::table)
                .values(values)
                .on_conflict(subscriptions::stripe_subscription_id)
                .do_update()
                .set(values)
                // The ordering guard: an event older than the one already applied
                // changes nothing. See the module docs.
                .filter(subscriptions::stripe_event_at.is_null().or(
                    subscriptions::stripe_event_at.le(excluded(subscriptions::stripe_event_at)),
                ))
                .execute(connection)
                .await
                .map_err(|source| write_failed(subscription, organization_id, source))?;

        if written == 0 {
            tracing::info!(
                billing.subscription.id = subscription.id,
                organization.id = %organization_id,
                "a Stripe event older than the one already applied left \
                 {{billing.subscription.id}} alone",
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

    /// The plan and interval a Stripe subscription buys.
    ///
    /// The price is the authority, because changing plan in the customer portal
    /// changes the price and leaves the metadata saying what was bought first.
    fn resolve_plan(&self, subscription: &stripe::Subscription) -> Result<(&str, Interval), Error> {
        let price_id = subscription.price_id().ok_or_else(|| {
            Error::new(
                ErrorKind::Data,
                format!("Stripe subscription {} has no price", subscription.id),
            )
        })?;

        if let Some((plan, interval)) = self.plans.find_by_price_id(price_id) {
            return Ok((plan.key(), interval));
        }

        // The price is not one `config/billing.yml` sells. The subscription may
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
                Ok((plan.key(), interval))
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
        // The only unique index this write can trip is the one live
        // subscription per organization, and hitting it means Stripe holds two.
        // A person has to decide which one the customer keeps.
        return Error::new(
            ErrorKind::Data,
            format!(
                "organization {organization_id} already holds a live subscription, so Stripe \
                 subscription {} could not be recorded; cancel one of them at Stripe",
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
struct Origin<'a> {
    /// The organization, when the caller resolved it already.
    organization_id: Option<Uuid>,
    /// A checkout session's metadata, when the subscription carries none.
    metadata: Option<&'a BTreeMap<String, String>>,
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
/// subscription occupying the organization's one live slot rather than letting a
/// second one start beside it.
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
