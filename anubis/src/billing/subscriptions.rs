//! Starting, changing, and cancelling subscriptions that pay for the application's records.
//!
//! An application that sells one subscription per thing (a server, a
//! workspace, a seat on something) calls [`Subscriptions`] from its own
//! handlers: [`Subscriptions::checkout`] starts one through Stripe Checkout,
//! [`Subscriptions::change_items`] adds, resizes, or removes line items
//! mid-period, and [`Subscriptions::cancel`] ends one. Each names the record it
//! pays for with a [`Subject`], and the subscription it creates carries that
//! subject in Stripe's metadata, so every event and every reconciliation
//! afterwards writes it back to the same row.
//!
//! ```ignore
//! let subscriptions = anubis::billing::Subscriptions::new(pool.clone(), plans.clone(), &config);
//! let server = anubis::billing::Subject::new("Server", server_id);
//!
//! // Start: the browser goes to Stripe, and the subscription arrives as an event.
//! let session = subscriptions
//!     .checkout(organization_id, NewSubscription {
//!         subject: &server,
//!         line_items: &[LineItem::new(&size.price_id, 1), LineItem::new(&region.price_id, 1)],
//!         return_path: &format!("/servers/{server_id}"),
//!         email: Some(&user.email),
//!     })
//!     .await?;
//!
//! // Change: an add-on mid-period, prorated by Stripe.
//! subscriptions
//!     .change_items(&context, organization_id, &server, &[ItemChange::set(&backups.price_id, 1)])
//!     .await?;
//!
//! // End: now, or at the end of the paid-for period.
//! subscriptions.cancel(&context, organization_id, &server, Cancellation::AtPeriodEnd).await?;
//! ```
//!
//! # Why this is a Rust API and not an endpoint
//!
//! Three questions decide whether a change to a subject subscription is
//! allowed, and only the application can answer any of them: whether the
//! subject belongs to the organization, who may spend money on it, and which
//! Stripe prices it is sold at. The last one is the sharpest: an endpoint that
//! took price ids from a browser would let a customer buy any price in the
//! Stripe account. So the application owns the routes, authorizes the caller
//! against its own record, computes the prices, and calls in here, which owns
//! everything about doing it correctly at Stripe. The framework's own `/billing`
//! endpoints list every subscription the organization holds, with its items,
//! and reconcile them; plan subscriptions keep their checkout and portal.
//!
//! Every method takes the organization explicitly, and a subject that
//! organization holds no live subscription for answers as not found, so a
//! record of another tenant's can never be changed through this one's.
//!
//! # Changes are serialized, read back, and keyed
//!
//! A change takes a row lock on the subscription for its whole length, so two
//! changes to one subscription run one after the other, each computed from
//! what the previous one left. Under the lock it reads the subscription back
//! from Stripe, works out the smallest request that reaches what was asked for
//! ([`ItemChange::set`] of a price already at that quantity sends nothing), and
//! sends it with an idempotency key naming the row's last write. A retry of a
//! change whose answer was lost is therefore one change at Stripe, and the
//! answer is written into the projection, with the change's audit event, before
//! the lock is released.
//!
//! The write is stamped when the read began, so an event describing anything
//! Stripe recorded later still applies; see [`super::lifecycle`]. If the local
//! write fails after Stripe has answered, the event Stripe sends about the
//! change repairs the projection, and so does reconciliation.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeSet;
use std::fmt::{self, Debug, Display, Formatter};

use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Object;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde_json::{Value, json};
use uuid::Uuid;

use super::lifecycle::{self, Origin, Reconciler, read_began};
use super::model::{
    Subject, Subscription, SubscriptionStatus, SubscriptionWithItems, terminal_statuses,
};
use super::plans::PlanSet;
use super::stripe::{
    self, CheckoutSession, ItemUpdate, LineItem, NewCheckoutSession, NewCustomer, Purchase,
    SubscriptionUpdate,
};
use crate::audit::{self, Changes};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::schema::{organizations, subscriptions};

/// The most line items one subscription may carry.
///
/// Stripe's own ceiling. Checked here as well, so an application asking for a
/// twenty-first item is told why rather than handed Stripe's refusal as a
/// server error.
const MAX_LINE_ITEMS: usize = 20;

/// Starts, changes, and cancels the subscriptions an application sells per record.
///
/// Cloning is cheap: the pool, the plans, and the HTTP client are all handles.
/// See the [module docs](self) for what it guarantees and why the application,
/// not the framework, serves the routes in front of it.
#[derive(Clone)]
pub struct Subscriptions {
    pool: DbPool,
    /// Writes every answer Stripe gives into the projection, the same way an
    /// event does.
    pub(super) reconciler: Reconciler,
    /// Absent until `STRIPE_SECRET_KEY` is set, which disables every write.
    pub(super) stripe: Option<stripe::Client>,
    app_url: String,
}

/// A subscription about to be bought for one of the application's records.
#[derive(Debug, Clone, Copy)]
pub struct NewSubscription<'a> {
    /// The record the subscription pays for.
    pub subject: &'a Subject,
    /// The prices it is sold at and how many of each, as the application
    /// computed them: never prices a browser named.
    pub line_items: &'a [LineItem],
    /// The application's own page Stripe returns the browser to, as a path
    /// under `APP_URL`, e.g. `/servers/1f0c...`. The outcome is appended as
    /// `?checkout=success` or `?checkout=canceled`.
    pub return_path: &'a str,
    /// Where Stripe sends receipts, when the organization becomes a Stripe
    /// customer with this purchase.
    pub email: Option<&'a str>,
}

/// One change to a subscription's line items, named by the price it bills.
///
/// Each says where an item should end up rather than what to do to it, so
/// asking twice is the same as asking once: setting a price already at that
/// quantity, or removing one the subscription does not bill, sends nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemChange {
    /// Bills `quantity` of the price: adds the item, or sets its quantity.
    Set {
        /// The Stripe price.
        price_id: String,
        /// How many of it; at least one.
        quantity: i64,
    },
    /// Stops billing the price, removing its item.
    Remove {
        /// The Stripe price.
        price_id: String,
    },
}

impl ItemChange {
    /// Bills `quantity` of `price_id`, adding the item when it is not there.
    #[must_use]
    pub fn set(price_id: impl Into<String>, quantity: i64) -> Self {
        Self::Set {
            price_id: price_id.into(),
            quantity,
        }
    }

    /// Stops billing `price_id`.
    #[must_use]
    pub fn remove(price_id: impl Into<String>) -> Self {
        Self::Remove {
            price_id: price_id.into(),
        }
    }

    fn price_id(&self) -> &str {
        match self {
            Self::Set { price_id, .. } | Self::Remove { price_id } => price_id,
        }
    }
}

/// When a cancelled subscription stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancellation {
    /// At the end of the period already paid for, which is what a customer
    /// who cancels expects. The subscription stays live until then.
    AtPeriodEnd,
    /// Immediately. Stripe's defaults apply: no credit for the unused part of
    /// the period, and no final invoice.
    Now,
}

/// What one locked change asks Stripe to do.
enum Intent<'a> {
    Items(&'a [ItemChange]),
    Cancel(Cancellation),
}

/// How a locked change ended, decided inside its transaction and acted on after it.
///
/// An ended subscription is not a rollback: its terminal state is written into
/// the projection and committed, and only then reported to the caller.
enum Outcome {
    /// The change applied, or was already in place; this is the row it wrote.
    Applied(Uuid),
    /// The subscription is over at Stripe, so there was nothing to change.
    Ended,
}

/// The request a change comes down to, once the current state is known.
enum Request<'a> {
    /// The subscription is already where the change would put it.
    Nothing,
    /// An update to its items or its cancellation date.
    Update(SubscriptionUpdate<'a>),
    /// Cancelling it now.
    Delete,
}

impl Subscriptions {
    /// Builds the service over the application's plans and Stripe credentials.
    #[must_use]
    pub fn new(pool: DbPool, plans: PlanSet, config: &AppConfig) -> Self {
        Self {
            reconciler: Reconciler::new(pool.clone(), plans, config),
            pool,
            stripe: config.stripe.as_ref().map(stripe::Client::new),
            app_url: config.app_url.clone(),
        }
    }

    /// Opens a Stripe Checkout session for a subscription paying for one record.
    ///
    /// Nothing is written here: the subscription exists at Stripe once the
    /// customer pays, and arrives as an event naming its subject. Creates the
    /// organization's Stripe customer on the way through, if this is its first
    /// purchase. The caller sends the browser to the session's URL.
    ///
    /// # Errors
    /// Returns an [`Error`] that is [`Error::is_unconfigured`] without
    /// `STRIPE_SECRET_KEY`, [`Error::is_invalid`] for line items Stripe would
    /// refuse or a return path that is not a path, [`Error::is_conflict`] when
    /// the subject already has a live subscription, and a Stripe or database
    /// failure otherwise.
    pub async fn checkout(
        &self,
        organization_id: Uuid,
        purchase: NewSubscription<'_>,
    ) -> Result<CheckoutSession, Error> {
        self.configured()?;
        validate_line_items(purchase.line_items)?;
        if !is_application_path(purchase.return_path) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "the return path {:?} must be a path under APP_URL, starting with one \
                     slash and carrying no query or fragment",
                    purchase.return_path,
                ),
            ));
        }

        let mut connection = self.connection().await?;
        // Across every organization, as the subject index is: a record is paid
        // for once. The index refuses a second live row anyway; asking first is
        // what keeps a second payment from being taken at all.
        let held: i64 = subscriptions::table
            .filter(subscriptions::subject_type.eq(purchase.subject.kind()))
            .filter(subscriptions::subject_id.eq(purchase.subject.id()))
            .filter(subscriptions::status.ne_all(terminal_statuses()))
            .count()
            .get_result(&mut connection)
            .await?;
        if held > 0 {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("{} already has a live subscription", purchase.subject),
            ));
        }
        drop(connection);

        let landing = format!("{}{}", self.app_url, purchase.return_path);
        self.open_checkout(
            organization_id,
            Purchase::Subject(purchase.subject),
            purchase.line_items,
            &landing,
            purchase.email,
        )
        .await
    }

    /// Adds, resizes, or removes line items on a subject's live subscription.
    ///
    /// All of `changes` go to Stripe as one update, so a swap (one price
    /// removed, another set) is one change with one set of prorations. Stripe's
    /// default proration applies: an item added mid-period is charged for the
    /// rest of it, and one removed earns a credit against the next invoice.
    /// Records `subscription.items_changed` in the audit log, attributed by
    /// `context`, when anything changed.
    ///
    /// Answers with the subscription and its items as Stripe left them.
    ///
    /// # Errors
    /// Returns an [`Error`] that is [`Error::is_not_found`] when the
    /// organization holds no live subscription for the subject,
    /// [`Error::is_invalid`] for a change Stripe would refuse (a price named
    /// twice, a quantity below one, no items left, more than twenty),
    /// [`Error::is_conflict`] when the subscription turns out to have ended at
    /// Stripe, [`Error::is_unconfigured`] without `STRIPE_SECRET_KEY`, and a
    /// Stripe or database failure otherwise.
    pub async fn change_items(
        &self,
        context: &audit::Context,
        organization_id: Uuid,
        subject: &Subject,
        changes: &[ItemChange],
    ) -> Result<SubscriptionWithItems, Error> {
        validate_changes(changes)?;
        self.locked_change(context, organization_id, subject, Intent::Items(changes))
            .await
    }

    /// Cancels a subject's live subscription, now or at the end of its period.
    ///
    /// Cancelling one already set to cancel at period end, or already over at
    /// Stripe, sends nothing and answers with what it found. Records
    /// `subscription.canceled` in the audit log when anything changed.
    ///
    /// # Errors
    /// Returns an [`Error`] that is [`Error::is_not_found`] when the
    /// organization holds no live subscription for the subject,
    /// [`Error::is_unconfigured`] without `STRIPE_SECRET_KEY`, and a Stripe or
    /// database failure otherwise.
    pub async fn cancel(
        &self,
        context: &audit::Context,
        organization_id: Uuid,
        subject: &Subject,
        when: Cancellation,
    ) -> Result<SubscriptionWithItems, Error> {
        self.locked_change(context, organization_id, subject, Intent::Cancel(when))
            .await
    }

    /// Opens a Checkout session for either kind of purchase.
    ///
    /// The one place a purchase starts, shared with the plan checkout endpoint,
    /// so the customer, the metadata, and the return links are built the same
    /// way for both.
    pub(super) async fn open_checkout(
        &self,
        organization_id: Uuid,
        purchase: Purchase<'_>,
        line_items: &[LineItem],
        landing: &str,
        email: Option<&str>,
    ) -> Result<CheckoutSession, Error> {
        let stripe = self.configured()?;
        let mut connection = self.connection().await?;
        let customer_id = ensure_customer(&mut connection, stripe, organization_id, email).await?;
        drop(connection);

        // `{CHECKOUT_SESSION_ID}` is a placeholder Stripe substitutes on the way
        // back, which is what lets the return page name the session that just
        // completed. The doubled braces escape it past this format string.
        let success_url = format!("{landing}?checkout=success&session_id={{CHECKOUT_SESSION_ID}}");
        let cancel_url = format!("{landing}?checkout=canceled");

        let session = stripe
            .create_checkout_session(NewCheckoutSession {
                customer_id: &customer_id,
                line_items,
                success_url: &success_url,
                cancel_url: &cancel_url,
                organization_id,
                purchase,
            })
            .await?;

        tracing::info!(
            organization.id = %organization_id,
            billing.checkout.session = %session.id,
            billing.line_items = line_items.len(),
            "checkout {{billing.checkout.session}} opened with {{billing.line_items}} line items",
        );

        Ok(session)
    }

    /// Runs one change under the subscription's row lock; see the module docs.
    async fn locked_change(
        &self,
        context: &audit::Context,
        organization_id: Uuid,
        subject: &Subject,
        intent: Intent<'_>,
    ) -> Result<SubscriptionWithItems, Error> {
        let stripe = self.configured()?;
        let mut connection = self.connection().await?;

        let outcome = connection
            .transaction::<_, Error, _>(async |connection| {
                // The lock serializes every change to this subscription, and a
                // webhook writing the same row waits for it too, then applies or
                // not by its own timestamp.
                let held: Subscription = subscriptions::table
                    .filter(subscriptions::organization_id.eq(organization_id))
                    .filter(subscriptions::subject_type.eq(subject.kind()))
                    .filter(subscriptions::subject_id.eq(subject.id()))
                    .filter(subscriptions::status.ne_all(terminal_statuses()))
                    .select(Subscription::as_select())
                    .for_update()
                    .first(connection)
                    .await
                    .optional()?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::NotFound,
                            format!(
                                "organization {organization_id} holds no live subscription for \
                                 {subject}",
                            ),
                        )
                    })?;

                let read_at = read_began();
                let current = stripe
                    .retrieve_subscription(&held.stripe_subscription_id)
                    .await?;
                let ended = SubscriptionStatus::parse(&current.status)
                    .is_some_and(SubscriptionStatus::is_terminal);

                let request = match (&intent, ended) {
                    // Already over: nothing to send, and the projection catches
                    // up below. A cancellation has its answer; a change has not.
                    (_intent, true) => Request::Nothing,
                    (Intent::Items(changes), false) => item_request(changes, &current)?,
                    (Intent::Cancel(when), false) => cancel_request(*when, &current),
                };

                let answered = match &request {
                    Request::Nothing => current.clone(),
                    // The row's last write is the revision: a retry of this
                    // change before anything is written sends the same key, and
                    // the same change asked for again after the projection has
                    // moved on sends a new one.
                    Request::Update(update) => {
                        stripe
                            .update_subscription(
                                &held.stripe_subscription_id,
                                update,
                                &held.updated_at.to_rfc3339(),
                            )
                            .await?
                    }
                    Request::Delete => {
                        stripe
                            .cancel_subscription(&held.stripe_subscription_id)
                            .await?
                    }
                };

                let origin = Origin {
                    organization_id: Some(organization_id),
                    metadata: None,
                };
                self.reconciler
                    .converge(connection, &answered, &origin, read_at)
                    .await?;

                if ended && matches!(intent, Intent::Items(_)) {
                    return Ok(Outcome::Ended);
                }
                if !matches!(request, Request::Nothing) {
                    record(
                        connection, context, &held, subject, &intent, &current, &answered,
                    )
                    .await?;
                }
                Ok(Outcome::Applied(held.id))
            })
            .await?;

        let Outcome::Applied(subscription_id) = outcome else {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("the subscription for {subject} has ended at Stripe"),
            ));
        };
        let row: Subscription = subscriptions::table
            .find(subscription_id)
            .select(Subscription::as_select())
            .first(&mut connection)
            .await?;
        let mut loaded = SubscriptionWithItems::load(&mut connection, vec![row]).await?;
        loaded.pop().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                format!("the subscription for {subject} vanished after it was changed"),
            )
        })
    }

    /// The Stripe client, or the honest answer that this deployment has none.
    fn configured(&self) -> Result<&stripe::Client, Error> {
        self.stripe.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::Unconfigured,
                "Billing is not configured for this deployment. Set STRIPE_SECRET_KEY to enable \
                 it.",
            )
        })
    }

    async fn connection(&self) -> Result<Object<AsyncPgConnection>, Error> {
        self.pool.get().await.map_err(|source| {
            Error::new(
                ErrorKind::Internal,
                format!("no database connection: {source}"),
            )
        })
    }
}

impl Debug for Subscriptions {
    /// Written by hand: the pool is not `Debug`, and the Stripe client must
    /// never render its key anyway.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscriptions")
            .field("stripe", &self.stripe.is_some())
            .finish_non_exhaustive()
    }
}

/// The smallest item update that puts `current` where `changes` ask.
///
/// Pure, so the arithmetic of what to send is tested without Stripe.
fn item_request<'a>(
    changes: &'a [ItemChange],
    current: &'a stripe::Subscription,
) -> Result<Request<'a>, Error> {
    let mut items = Vec::new();
    let mut remaining = current.items().count();

    for change in changes {
        let existing = current.item_for_price(change.price_id());
        match (change, existing) {
            (ItemChange::Set { quantity, .. }, Some(item))
                if item.quantity() == Some(*quantity) => {}
            (ItemChange::Set { quantity, .. }, Some(item)) => items.push(ItemUpdate::SetQuantity {
                item_id: item.id(),
                quantity: *quantity,
            }),
            (ItemChange::Set { price_id, quantity }, None) => {
                items.push(ItemUpdate::Add {
                    price_id,
                    quantity: *quantity,
                });
                remaining += 1;
            }
            (ItemChange::Remove { .. }, Some(item)) => {
                items.push(ItemUpdate::Remove { item_id: item.id() });
                remaining -= 1;
            }
            (ItemChange::Remove { .. }, None) => {}
        }
    }

    if remaining == 0 {
        return Err(Error::new(
            ErrorKind::Invalid,
            "the change would leave the subscription with no line items; cancel it instead",
        ));
    }
    if remaining > MAX_LINE_ITEMS {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("a subscription carries at most {MAX_LINE_ITEMS} line items"),
        ));
    }
    if items.is_empty() {
        return Ok(Request::Nothing);
    }

    Ok(Request::Update(SubscriptionUpdate {
        items,
        cancel_at_period_end: None,
    }))
}

/// The request a cancellation comes down to, given where the subscription is.
fn cancel_request(when: Cancellation, current: &stripe::Subscription) -> Request<'static> {
    match when {
        Cancellation::AtPeriodEnd if current.cancel_at_period_end => Request::Nothing,
        Cancellation::AtPeriodEnd => Request::Update(SubscriptionUpdate {
            items: Vec::new(),
            cancel_at_period_end: Some(true),
        }),
        Cancellation::Now => Request::Delete,
    }
}

/// Records one change in the audit log, inside the transaction that wrote it.
async fn record(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    held: &Subscription,
    subject: &Subject,
    intent: &Intent<'_>,
    before: &stripe::Subscription,
    after: &stripe::Subscription,
) -> Result<(), Error> {
    let (action, changes) = match intent {
        Intent::Items(_changes) => (
            audit::SUBSCRIPTION_ITEMS_CHANGED,
            Changes::new().field("items", item_summary(before), item_summary(after)),
        ),
        Intent::Cancel(Cancellation::AtPeriodEnd) => (
            audit::SUBSCRIPTION_CANCELED,
            Changes::new().field(
                "cancel_at_period_end",
                before.cancel_at_period_end,
                after.cancel_at_period_end,
            ),
        ),
        Intent::Cancel(Cancellation::Now) => (
            audit::SUBSCRIPTION_CANCELED,
            Changes::new().field("status", before.status.as_str(), after.status.as_str()),
        ),
    };
    let label = subject.to_string();

    audit::record(
        connection,
        context,
        &audit::Event::new(action, SUBSCRIPTION_SUBJECT)
            .organization(held.organization_id)
            .subject(held.id)
            .label(&label)
            .changes(changes),
    )
    .await?;
    Ok(())
}

/// How the audit log names a subscription as the thing an act happened to.
const SUBSCRIPTION_SUBJECT: &str = "Subscription";

/// A subscription's items as the audit log records them: price and quantity.
fn item_summary(subscription: &stripe::Subscription) -> Value {
    subscription
        .items()
        .map(|item| json!({ "price": item.price_id(), "quantity": item.quantity() }))
        .collect()
}

/// Refuses line items Stripe would refuse, so the reason reaches the caller.
fn validate_line_items(line_items: &[LineItem]) -> Result<(), Error> {
    if line_items.is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "a subscription needs at least one line item",
        ));
    }
    if line_items.len() > MAX_LINE_ITEMS {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("a subscription carries at most {MAX_LINE_ITEMS} line items"),
        ));
    }

    let mut seen = BTreeSet::new();
    for item in line_items {
        if item.price_id().trim().is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "a line item names no price"));
        }
        if item.quantity() < 1 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "the line item for {} asks for fewer than one",
                    item.price_id()
                ),
            ));
        }
        if !seen.insert(item.price_id()) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "{} is named twice; a subscription bills a price once",
                    item.price_id()
                ),
            ));
        }
    }

    Ok(())
}

/// Refuses item changes Stripe would refuse, before anything is locked.
fn validate_changes(changes: &[ItemChange]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for change in changes {
        if change.price_id().trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "an item change names no price",
            ));
        }
        if matches!(change, ItemChange::Set { quantity, .. } if *quantity < 1) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "the change to {} asks for fewer than one; remove it instead",
                    change.price_id(),
                ),
            ));
        }
        if !seen.insert(change.price_id()) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("{} is changed twice in one request", change.price_id()),
            ));
        }
    }

    Ok(())
}

/// Whether a return path stays on this application.
///
/// One leading slash and nothing that changes where a URL points once it is
/// appended to `APP_URL`: a second slash would make it a different host, and a
/// query or fragment would break the outcome appended after it.
fn is_application_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['?', '#', '\\'])
        && !path.chars().any(char::is_whitespace)
}

/// The organization's Stripe customer, creating one on first use.
///
/// The write is conditional on the column still being null, so two checkouts
/// started at the same instant cannot overwrite each other. They cannot even
/// create two customers: the create call carries an idempotency key derived
/// from the organization, so Stripe answers the second one with the customer
/// the first one made.
async fn ensure_customer(
    connection: &mut AsyncPgConnection,
    stripe: &stripe::Client,
    organization_id: Uuid,
    email: Option<&str>,
) -> Result<String, Error> {
    if let Some(existing) = stored_customer_id(connection, organization_id).await? {
        return Ok(existing);
    }

    let name: String = organizations::table
        .find(organization_id)
        .select(organizations::name)
        .first(connection)
        .await?;
    let customer = stripe
        .create_customer(NewCustomer {
            organization_id,
            name: &name,
            email,
        })
        .await?;

    diesel::update(
        organizations::table
            .find(organization_id)
            .filter(organizations::stripe_customer_id.is_null()),
    )
    .set(organizations::stripe_customer_id.eq(&customer.id))
    .execute(connection)
    .await?;

    // Re-read rather than returning what was just created: whoever won the
    // write is the customer this organization is billed as.
    stored_customer_id(connection, organization_id)
        .await?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                format!(
                    "organization {organization_id} vanished between creating its Stripe \
                     customer and storing it",
                ),
            )
        })
}

/// The Stripe customer stored on an organization, if it has one.
pub(super) async fn stored_customer_id(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Option<String>> {
    organizations::table
        .find(organization_id)
        .select(organizations::stripe_customer_id)
        .first::<Option<String>>(connection)
        .await
        .optional()
        .map(Option::flatten)
}

/// What went wrong starting, changing, or cancelling a subscription.
#[derive(Debug)]
enum ErrorKind {
    /// Billing is not configured, so Stripe cannot be reached at all.
    Unconfigured,
    /// The organization holds no live subscription for the subject.
    NotFound,
    /// The request is one Stripe would refuse.
    Invalid,
    /// The subject already has a subscription, or its subscription has ended.
    Conflict,
    /// Stripe could not be reached; worth retrying.
    Unavailable,
    /// Stripe refused, or the database did, or Stripe said something unusable.
    Internal,
}

/// A subscription that could not be started, changed, or cancelled.
///
/// The `is_*` methods say which, and the [`From`] into [`ApiError`] applies
/// them, so an application's handler forwards one with `?`: not found is
/// `404`, invalid `400`, a conflict `409`, billing unconfigured or Stripe
/// unreachable `503`, and anything else `500` with the detail in the log. The
/// message never carries a credential: the Stripe key travels in a header and
/// no part of a header reaches here.
pub struct Error {
    kind: ErrorKind,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// Returns `true` when this deployment has no Stripe credentials.
    #[must_use]
    pub fn is_unconfigured(&self) -> bool {
        matches!(self.kind, ErrorKind::Unconfigured)
    }

    /// Returns `true` when the organization holds no live subscription for the subject.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self.kind, ErrorKind::NotFound)
    }

    /// Returns `true` when the request is one Stripe would refuse.
    #[must_use]
    pub fn is_invalid(&self) -> bool {
        matches!(self.kind, ErrorKind::Invalid)
    }

    /// Returns `true` when the subject already has a live subscription, or its
    /// subscription has ended.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        matches!(self.kind, ErrorKind::Conflict)
    }

    /// Returns `true` when Stripe could not be reached, so retrying may work.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Unavailable)
    }

    /// The failure on its own, without the backtrace [`Display`] adds.
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
}

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl From<stripe::Error> for Error {
    fn from(source: stripe::Error) -> Self {
        let kind = if source.is_transport() {
            ErrorKind::Unavailable
        } else {
            ErrorKind::Internal
        };
        Self::new(kind, source.to_string())
    }
}

impl From<lifecycle::Error> for Error {
    fn from(source: lifecycle::Error) -> Self {
        let kind = if source.is_transport() {
            ErrorKind::Unavailable
        } else if source.is_unconfigured() {
            ErrorKind::Unconfigured
        } else {
            ErrorKind::Internal
        };
        Self::new(kind, source.message())
    }
}

impl From<diesel::result::Error> for Error {
    fn from(source: diesel::result::Error) -> Self {
        Self::new(
            ErrorKind::Internal,
            format!("the database refused: {source}"),
        )
    }
}

impl From<Error> for ApiError {
    /// Renders a subscription failure as the answer a caller acts on.
    fn from(source: Error) -> Self {
        match source.kind {
            ErrorKind::NotFound => Self::not_found(),
            ErrorKind::Invalid => Self::validation(source.message),
            ErrorKind::Conflict => Self::conflict(source.message),
            ErrorKind::Unconfigured => Self::unavailable(source.message),
            ErrorKind::Unavailable => {
                tracing::warn!(
                    error.message = source.message,
                    "Stripe was unreachable for a subscription request: {{error.message}}",
                );
                Self::unavailable("Billing is temporarily unavailable. Try again in a moment.")
            }
            ErrorKind::Internal => {
                tracing::error!(
                    error.message = source.message,
                    "a subscription request failed: {{error.message}}",
                );
                Self::internal()
            }
        }
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
    use axum::http::StatusCode;

    use super::{
        Cancellation, Error, ErrorKind, ItemChange, Request, cancel_request, is_application_path,
        item_request, validate_changes, validate_line_items,
    };
    use crate::billing::LineItem;
    use crate::billing::stripe::{self, ItemUpdate};
    use crate::http::ApiError;

    /// A Stripe subscription billing a server size and a backup add-on.
    fn server() -> stripe::Subscription {
        serde_json::from_value(serde_json::json!({
            "id": "sub_server", "status": "active", "customer": "cus_1",
            "cancel_at_period_end": false,
            "items": { "data": [
                { "id": "si_size", "quantity": 1,
                  "price": { "id": "price_size_small", "recurring": { "interval": "month" } } },
                { "id": "si_backups", "quantity": 1,
                  "price": { "id": "price_backups", "recurring": { "interval": "month" } } },
            ] },
        }))
        .expect("the subscription must parse")
    }

    fn updates(request: Request<'_>) -> Vec<ItemUpdate<'_>> {
        match request {
            Request::Update(update) => update.items,
            Request::Nothing => Vec::new(),
            Request::Delete => panic!("an item change never deletes the subscription"),
        }
    }

    #[test]
    fn a_new_price_is_added_and_a_known_one_is_resized_by_its_item() {
        let current = server();
        let changes = [
            ItemChange::set("price_logs", 1),
            ItemChange::set("price_backups", 3),
        ];

        let sent = updates(item_request(&changes, &current).expect("the change is valid"));

        assert_eq!(
            sent,
            vec![
                ItemUpdate::Add {
                    price_id: "price_logs",
                    quantity: 1,
                },
                ItemUpdate::SetQuantity {
                    item_id: "si_backups",
                    quantity: 3,
                },
            ],
        );
    }

    #[test]
    fn asking_for_what_is_already_there_sends_nothing() {
        let current = server();
        let changes = [
            ItemChange::set("price_backups", 1),
            ItemChange::remove("price_never_billed"),
        ];

        let request = item_request(&changes, &current).expect("the change is valid");

        assert!(matches!(request, Request::Nothing), "a retry is a no-op");
    }

    #[test]
    fn a_swap_is_one_request_naming_the_removed_item_by_its_id() {
        let current = server();
        let changes = [
            ItemChange::remove("price_size_small"),
            ItemChange::set("price_size_large", 1),
        ];

        let sent = updates(item_request(&changes, &current).expect("the change is valid"));

        assert_eq!(
            sent,
            vec![
                ItemUpdate::Remove { item_id: "si_size" },
                ItemUpdate::Add {
                    price_id: "price_size_large",
                    quantity: 1,
                },
            ],
        );
    }

    #[test]
    fn removing_every_item_is_refused_in_favor_of_cancelling() {
        let current = server();
        let changes = [
            ItemChange::remove("price_size_small"),
            ItemChange::remove("price_backups"),
        ];

        let refused = item_request(&changes, &current)
            .err()
            .expect("an empty subscription is not a change");

        assert!(refused.is_invalid());
        assert!(refused.message().contains("cancel"), "got: {refused}");
    }

    #[test]
    fn a_cancellation_already_scheduled_sends_nothing() {
        let mut current = server();
        assert!(matches!(
            cancel_request(Cancellation::AtPeriodEnd, &current),
            Request::Update(_),
        ));

        current.cancel_at_period_end = true;
        assert!(matches!(
            cancel_request(Cancellation::AtPeriodEnd, &current),
            Request::Nothing,
        ));
        assert!(matches!(
            cancel_request(Cancellation::Now, &current),
            Request::Delete,
        ));
    }

    #[test]
    fn changes_stripe_would_refuse_are_refused_first() {
        let refused_changes = [
            vec![ItemChange::set("price_a", 0)],
            vec![ItemChange::set(" ", 1)],
            // One price changed twice in one request is ambiguous.
            vec![ItemChange::set("price_a", 1), ItemChange::remove("price_a")],
        ];
        for changes in refused_changes {
            let refused = validate_changes(&changes).expect_err("the change must be refused");
            assert!(refused.is_invalid(), "{changes:?}: {refused}");
        }
        validate_changes(&[ItemChange::set("price_a", 2)]).expect("a quantity of two is fine");

        let too_many: Vec<LineItem> = (0..21)
            .map(|index| LineItem::new(format!("price_{index}"), 1))
            .collect();
        let refused_items = [
            Vec::new(),
            vec![LineItem::new("price_a", 0)],
            vec![LineItem::new("price_a", 1), LineItem::new("price_a", 2)],
            too_many,
        ];
        for items in refused_items {
            let refused = validate_line_items(&items).expect_err("the items must be refused");
            assert!(refused.is_invalid(), "{items:?}: {refused}");
        }
        validate_line_items(&[LineItem::new("price_a", 1)]).expect("one line is fine");
    }

    #[test]
    fn a_return_path_cannot_leave_the_application() {
        assert!(is_application_path("/servers/1f0c"));
        for escaping in [
            "servers/1f0c",
            "//evil.example.com/servers",
            "/\\evil.example.com",
            "/servers?checkout=hijacked",
            "/servers#fragment",
            "/servers /1f0c",
            "",
        ] {
            assert!(
                !is_application_path(escaping),
                "{escaping:?} must be refused"
            );
        }
    }

    #[test]
    fn each_failure_answers_with_the_status_a_caller_acts_on() {
        let cases = [
            (ErrorKind::NotFound, StatusCode::NOT_FOUND),
            (ErrorKind::Invalid, StatusCode::BAD_REQUEST),
            (ErrorKind::Conflict, StatusCode::CONFLICT),
            (ErrorKind::Unconfigured, StatusCode::SERVICE_UNAVAILABLE),
            (ErrorKind::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
            (ErrorKind::Internal, StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (kind, status) in cases {
            let answered: ApiError = Error::new(kind, "the detail").into();
            assert_eq!(answered.status(), status);
        }

        let hidden: ApiError = Error::new(ErrorKind::Internal, "socket closed").into();
        assert!(
            !hidden.message().contains("socket"),
            "the cause belongs in the log, not the body",
        );
    }
}
