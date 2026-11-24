//! The handful of Stripe API calls the billing module makes.
//!
//! Anubis talks to Stripe over its REST API directly, with [`Client`], rather
//! than through a generated SDK. The reasoning is in `docs/billing.md`, and it
//! comes down to surface area: the whole integration is eight calls, five
//! writes and three reads, every one of them answering flat JSON. The
//! framework already carries a `reqwest` client built on rustls for outgoing
//! webhooks, so this module adds no dependency, no second TLS stack, and no
//! compile time worth measuring.
//!
//! # What is here
//!
//! | Call | Stripe endpoint | Used by |
//! |---|---|---|
//! | [`Client::create_customer`] | `POST /v1/customers` | The first checkout an organization starts |
//! | [`Client::create_checkout_session`] | `POST /v1/checkout/sessions` | Starting a subscription |
//! | [`Client::create_portal_session`] | `POST /v1/billing_portal/sessions` | Managing an existing one |
//! | [`Client::update_subscription`] | `POST /v1/subscriptions/{id}` | Changing line items, seats, and cancelling at period end |
//! | [`Client::cancel_subscription`] | `DELETE /v1/subscriptions/{id}` | Cancelling a subscription now |
//! | [`Client::retrieve_subscription`] | `GET /v1/subscriptions/{id}` | Reading current state when an event arrives |
//! | [`Client::list_subscriptions`] | `GET /v1/subscriptions` | Reconciling an organization against Stripe |
//! | [`Client::complete_items`] | `GET /v1/subscription_items` | Reading the rest of a long item list |
//!
//! The reads exist because an event's body is a snapshot of the moment it was
//! created, and a job may process it minutes later. Stripe's own guidance is to
//! treat a payload as possibly stale and read the object back, which is what
//! [`crate::billing::Reconciler`] does; see `docs/billing.md`.
//!
//! # Conventions
//!
//! Requests are `application/x-www-form-urlencoded`, as Stripe's API expects,
//! with nested parameters spelled `metadata[organization_id]` and
//! `line_items[0][price]`. Responses are parsed into the few fields the
//! framework reads; Stripe adds fields constantly, and a struct that insisted
//! on knowing all of them would break on their schedule rather than ours.
//!
//! Creating a customer carries an [idempotency key][idempotency] derived from
//! the organization, so two checkouts started at the same instant converge on
//! one customer instead of racing to create two. An update to a subscription
//! carries one too, derived from the request and from a *revision* the caller
//! names: the state the change was computed from. A retry of one change is
//! therefore one change at Stripe, and the same change asked for again after
//! the subscription moved on is a second one. Cancelling needs none, because
//! Stripe treats a `DELETE` as idempotent by definition.
//!
//! No `Stripe-Version` header is sent, so calls run on the account's default
//! API version. Pinning one is a single constant here when a deployment wants
//! its upgrades to be deliberate.
//!
//! [idempotency]: https://docs.stripe.com/api/idempotent_requests

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter, Write as _};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::model::Subject;
use crate::config::StripeConfig;

/// Stripe's own API host, and the default for `STRIPE_API_BASE`.
pub const API_BASE: &str = "https://api.stripe.com";

/// How long one Stripe call may take before it counts as a failure.
///
/// Stripe answers these three calls in well under a second in normal
/// operation. The generous ceiling is for the day it does not: a checkout that
/// hangs holds a browser tab, and the caller would rather be told to try again.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest Stripe message kept on an error.
///
/// Long enough for any real API message, short enough that an unexpected body
/// cannot fill a log line.
const MAX_MESSAGE_LENGTH: usize = 2_000;

/// How many records one page of a Stripe list carries.
///
/// Stripe's own maximum, so reading a long list costs as few calls as it can.
const PAGE_SIZE: &str = "100";

/// The most pages one listing of a customer's subscriptions follows.
///
/// An organization that pays per record can hold hundreds of subscriptions
/// over its life, so a listing pages through them rather than stopping at the
/// first hundred. The ceiling, ten thousand subscriptions, is a guard against a
/// list that never reports its end rather than a size anybody reaches: hitting
/// it is an error, never a silent truncation.
const MAX_LIST_PAGES: usize = 100;

/// A Stripe API client, holding the secret key and the base URL.
///
/// Cloning is cheap: the HTTP client is a handle, and the rest is two strings.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    secret_key: String,
    base_url: String,
}

impl Client {
    /// Builds a client from the application's Stripe configuration.
    ///
    /// # Panics
    /// Panics if the HTTP client cannot be built, which means the process has
    /// no usable TLS backend and nothing it does over the network would work.
    #[must_use]
    pub fn new(config: &StripeConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("the HTTP client must build"),
            secret_key: config.secret_key().to_owned(),
            base_url: config.api_base().to_owned(),
        }
    }

    /// Creates the Stripe customer an organization is billed as.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn create_customer(&self, customer: NewCustomer<'_>) -> Result<Customer, Error> {
        let mut form = Form::new();
        form.text("name", customer.name);
        if let Some(email) = customer.email {
            form.text("email", email);
        }
        form.text(
            "metadata[organization_id]",
            &customer.organization_id.to_string(),
        );

        // The key makes a retry, or a second concurrent checkout, return the
        // customer the first call created rather than making another one.
        let key = format!("anubis-customer-{}", customer.organization_id);
        self.post("/v1/customers", &form, Some(&key)).await
    }

    /// Opens a Stripe Checkout session for one subscription purchase.
    ///
    /// The session carries the organization, and the plan or subject being
    /// bought, in its metadata and on the subscription it creates, so the
    /// events that follow identify the tenant and what it pays for without a
    /// lookup table of Stripe ids.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn create_checkout_session(
        &self,
        session: NewCheckoutSession<'_>,
    ) -> Result<CheckoutSession, Error> {
        let organization_id = session.organization_id.to_string();

        let mut form = Form::new();
        form.text("mode", "subscription");
        form.text("customer", session.customer_id);
        for (index, item) in session.line_items.iter().enumerate() {
            form.text(&format!("line_items[{index}][price]"), item.price_id());
            form.text(
                &format!("line_items[{index}][quantity]"),
                &item.quantity().to_string(),
            );
        }
        form.text("success_url", session.success_url);
        form.text("cancel_url", session.cancel_url);
        // Stripe echoes this on the completed-session event; the metadata below
        // rides the subscription itself, which is what every later event names.
        form.text("client_reference_id", &organization_id);

        let mut metadata = vec![(ORGANIZATION_METADATA, organization_id)];
        match session.purchase {
            Purchase::Plan(plan_key) => metadata.push((PLAN_METADATA, plan_key.to_owned())),
            Purchase::Subject(subject) => {
                metadata.push((SUBJECT_TYPE_METADATA, subject.kind().to_owned()));
                metadata.push((SUBJECT_ID_METADATA, subject.id().to_string()));
            }
        }
        for (name, value) in &metadata {
            form.text(&format!("metadata[{name}]"), value);
            form.text(&format!("subscription_data[metadata][{name}]"), value);
        }

        self.post("/v1/checkout/sessions", &form, None).await
    }

    /// Opens a Stripe Billing customer portal session.
    ///
    /// The portal is where a customer changes plan, changes card, and cancels;
    /// the application never builds those screens, it links to this URL.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn create_portal_session(
        &self,
        session: NewPortalSession<'_>,
    ) -> Result<PortalSession, Error> {
        let mut form = Form::new();
        form.text("customer", session.customer_id);
        form.text("return_url", session.return_url);

        self.post("/v1/billing_portal/sessions", &form, None).await
    }

    /// Reads one subscription's current state.
    ///
    /// This is how an event is turned into something worth writing down: the
    /// event says which subscription changed, and this says what it looks like
    /// now, which is not always what the event's own body said.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    /// A subscription Stripe has never heard of is an API refusal carrying the
    /// code `resource_missing`.
    pub async fn retrieve_subscription(&self, id: &str) -> Result<Subscription, Error> {
        // Percent-encoded because the id reaches this call from an event body,
        // and a path segment is never a place to paste unescaped input.
        let path = format!("/v1/subscriptions/{}", encode_path_segment(id));
        let subscription = self.get(&path, &[]).await?;
        self.complete_items(subscription).await
    }

    /// Changes a subscription's line items, or sets it to cancel at period end.
    ///
    /// Every item change rides one request, so a swap (one price removed,
    /// another added) is one change with one set of prorations rather than two.
    /// Nothing about proration is sent, which leaves Stripe's own default of
    /// `create_prorations`: an item or a seat added mid-period is charged for
    /// the rest of that period, and one removed earns a credit against the next
    /// invoice. Changing that is an account-level decision rather than a
    /// framework one.
    ///
    /// `revision` names the state the change was computed from, and keys the
    /// request at Stripe together with the change itself; see the module docs.
    ///
    /// Answers with the subscription as it stands afterwards, which is what the
    /// caller writes into the projection.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn update_subscription(
        &self,
        subscription_id: &str,
        update: &SubscriptionUpdate<'_>,
        revision: &str,
    ) -> Result<Subscription, Error> {
        let mut form = Form::new();
        for (index, change) in update.items.iter().enumerate() {
            match *change {
                ItemUpdate::Add { price_id, quantity } => {
                    form.text(&format!("items[{index}][price]"), price_id);
                    form.text(&format!("items[{index}][quantity]"), &quantity.to_string());
                }
                ItemUpdate::SetQuantity { item_id, quantity } => {
                    form.text(&format!("items[{index}][id]"), item_id);
                    form.text(&format!("items[{index}][quantity]"), &quantity.to_string());
                }
                ItemUpdate::Remove { item_id } => {
                    form.text(&format!("items[{index}][id]"), item_id);
                    form.text(&format!("items[{index}][deleted]"), "true");
                }
            }
        }
        if let Some(cancel) = update.cancel_at_period_end {
            form.text(
                "cancel_at_period_end",
                if cancel { "true" } else { "false" },
            );
        }

        // Percent-encoded for the same reason `retrieve_subscription` does it:
        // the id reaches here from a stored row that an event body wrote.
        let path = format!("/v1/subscriptions/{}", encode_path_segment(subscription_id));
        let key = idempotency_key("POST", &path, &form.finish(), revision);
        let subscription = self.post(&path, &form, Some(&key)).await?;
        self.complete_items(subscription).await
    }

    /// Cancels a subscription now, rather than at the end of its period.
    ///
    /// Nothing is sent beside the request, which leaves Stripe's defaults: no
    /// credit for the unused part of the period and no final invoice. Both are
    /// the account's policy to choose, not the framework's. No idempotency key
    /// either: Stripe ignores one on a `DELETE`, which is idempotent by
    /// definition, since a cancelled subscription cannot be cancelled twice.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn cancel_subscription(&self, subscription_id: &str) -> Result<Subscription, Error> {
        let path = format!("/v1/subscriptions/{}", encode_path_segment(subscription_id));
        let request = self
            .http
            .delete(format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret_key);

        let subscription = self.send(request, &path).await?;
        self.complete_items(subscription).await
    }

    /// Lists every subscription a customer has ever held, newest first.
    ///
    /// `status=all` is deliberate: reconciliation has to see the cancelled ones
    /// too, or a row this application still believes is live would have nothing
    /// to correct it. The listing follows Stripe's pages to the end, because a
    /// customer paying per record holds a subscription per record.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached,
    /// and when the list runs past [`MAX_LIST_PAGES`] pages.
    pub async fn list_subscriptions(&self, customer_id: &str) -> Result<Vec<Subscription>, Error> {
        let path = "/v1/subscriptions";
        let mut held = Vec::new();
        let mut after: Option<String> = None;

        for _page in 0..MAX_LIST_PAGES {
            let mut query = vec![
                ("customer", customer_id),
                ("status", "all"),
                ("limit", PAGE_SIZE),
            ];
            if let Some(last) = after.as_deref() {
                query.push(("starting_after", last));
            }

            let page: List<Subscription> = self.get(path, &query).await?;
            after = page.data.last().map(|subscription| subscription.id.clone());
            for subscription in page.data {
                held.push(self.complete_items(subscription).await?);
            }
            if !page.has_more || after.is_none() {
                return Ok(held);
            }
        }

        Err(Error::decode(
            path,
            &format!("the list did not end within {MAX_LIST_PAGES} pages"),
        ))
    }

    /// Fills in a subscription's line items when Stripe sent only the first page.
    ///
    /// A subscription embeds its items as a list, and a long one arrives with
    /// `has_more` set. The projection deletes the items Stripe no longer
    /// reports, so writing a partial list would delete items that still exist;
    /// this reads the rest first. A subscription holds at most twenty items, so
    /// one page of the item list is all of them. Returns the subscription
    /// untouched, without a call, when its list is already whole, which is the
    /// common case.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn complete_items(
        &self,
        mut subscription: Subscription,
    ) -> Result<Subscription, Error> {
        if !subscription.items.has_more {
            return Ok(subscription);
        }

        let items: List<SubscriptionItem> = self
            .get(
                "/v1/subscription_items",
                &[("subscription", &subscription.id), ("limit", PAGE_SIZE)],
            )
            .await?;
        subscription.items = SubscriptionItems {
            data: items.data,
            has_more: items.has_more,
        };

        Ok(subscription)
    }

    /// POSTs a form to one Stripe endpoint and parses what comes back.
    ///
    /// The one place the secret key is used, so it is also the one place that
    /// has to keep it out of errors and logs.
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &Form,
        idempotency_key: Option<&str>,
    ) -> Result<T, Error> {
        let url = format!("{}{path}", self.base_url);
        let mut request = self
            .http
            .post(&url)
            .bearer_auth(&self.secret_key)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form.finish());
        if let Some(key) = idempotency_key {
            request = request.header(IDEMPOTENCY_HEADER, key);
        }

        self.send(request, path).await
    }

    /// GETs one Stripe endpoint with `query` and parses what comes back.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        let request = self
            .http
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret_key)
            .query(query);

        self.send(request, path).await
    }

    /// Sends one prepared request and reads Stripe's answer.
    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<T, Error> {
        let response = request
            .send()
            .await
            .map_err(|source| Error::transport(&source))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|source| Error::transport(&source))?;

        if !status.is_success() {
            return Err(Error::api(status.as_u16(), &body, path));
        }
        serde_json::from_str(&body).map_err(|source| Error::decode(path, &source.to_string()))
    }
}

impl Debug for Client {
    /// Written by hand: the secret key must never render itself.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("timeout", &REQUEST_TIMEOUT)
            .finish_non_exhaustive()
    }
}

/// The customer an organization is billed as.
#[derive(Debug, Clone)]
pub struct NewCustomer<'a> {
    /// The organization being billed, carried into Stripe's metadata.
    pub organization_id: Uuid,
    /// The customer's name, which is the organization's.
    pub name: &'a str,
    /// Where Stripe sends receipts, when the application knows an address.
    pub email: Option<&'a str>,
}

/// Stripe's metadata key naming the organization a subscription belongs to.
pub(super) const ORGANIZATION_METADATA: &str = "organization_id";

/// Stripe's metadata key naming the plan a checkout bought.
pub(super) const PLAN_METADATA: &str = "plan_key";

/// Stripe's metadata key naming the model of the record a subscription pays for.
pub(super) const SUBJECT_TYPE_METADATA: &str = "subject_type";

/// Stripe's metadata key naming the id of the record a subscription pays for.
pub(super) const SUBJECT_ID_METADATA: &str = "subject_id";

/// The header Stripe deduplicates writes by.
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// One subscription purchase, about to be handed to Stripe Checkout.
#[derive(Debug, Clone)]
pub struct NewCheckoutSession<'a> {
    /// The Stripe customer paying, e.g. `cus_1Q...`.
    pub customer_id: &'a str,
    /// The prices being bought and how many of each: one line for a plan, as
    /// many as the application sells for a subject.
    pub line_items: &'a [LineItem],
    /// Where Stripe returns the browser after a completed purchase.
    pub success_url: &'a str,
    /// Where Stripe returns the browser when the customer backs out.
    pub cancel_url: &'a str,
    /// The organization being subscribed, carried into Stripe's metadata.
    pub organization_id: Uuid,
    /// What the subscription buys, carried into Stripe's metadata.
    pub purchase: Purchase<'a>,
}

/// What a subscription buys: the organization's plan, or one of its records.
///
/// One or the other, never both, which is the rule the `subscriptions` table
/// holds as a check constraint; a type of its own makes the third shape
/// unwritable here too.
#[derive(Debug, Clone, Copy)]
pub enum Purchase<'a> {
    /// A plan from `config/billing.yml`, named by its key.
    Plan(&'a str),
    /// One of the application's records.
    Subject(&'a Subject),
}

/// A Stripe price and how many of it: one line of a purchase.
///
/// # Examples
/// ```
/// use anubis::billing::LineItem;
///
/// let cores = LineItem::new("price_core_monthly", 2);
/// assert_eq!(cores.price_id(), "price_core_monthly");
/// assert_eq!(cores.quantity(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineItem {
    price_id: String,
    quantity: i64,
}

impl LineItem {
    /// One line: `quantity` of the Stripe price `price_id`.
    #[must_use]
    pub fn new(price_id: impl Into<String>, quantity: i64) -> Self {
        Self {
            price_id: price_id.into(),
            quantity,
        }
    }

    /// The Stripe price, e.g. `price_1Q...`.
    #[must_use]
    pub fn price_id(&self) -> &str {
        &self.price_id
    }

    /// How many of the price.
    #[must_use]
    pub fn quantity(&self) -> i64 {
        self.quantity
    }
}

/// A change to one subscription, as [`Client::update_subscription`] sends it.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionUpdate<'a> {
    /// Line-item changes, applied together in one request.
    pub items: Vec<ItemUpdate<'a>>,
    /// `Some(true)` sets the subscription to cancel at the end of its period.
    pub cancel_at_period_end: Option<bool>,
}

impl SubscriptionUpdate<'_> {
    /// Whether the update would send nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.cancel_at_period_end.is_none()
    }
}

/// One line-item change, in the terms Stripe's update takes.
///
/// An item that exists is named by its Stripe item id; a new one by its price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemUpdate<'a> {
    /// Adds a new line item for a price the subscription does not bill yet.
    Add {
        /// The Stripe price to add.
        price_id: &'a str,
        /// How many of it.
        quantity: i64,
    },
    /// Sets how many of an existing item's price the subscription bills.
    SetQuantity {
        /// Stripe's id for the item, e.g. `si_1Q...`.
        item_id: &'a str,
        /// The new quantity.
        quantity: i64,
    },
    /// Removes an existing item.
    Remove {
        /// Stripe's id for the item, e.g. `si_1Q...`.
        item_id: &'a str,
    },
}

/// The idempotency key for one write, from the request and its revision.
///
/// Hashed rather than concatenated, so a key stays inside Stripe's 255
/// characters however many items the request names, and names nothing about
/// the customer to whoever reads Stripe's request log.
fn idempotency_key(method: &str, path: &str, body: &str, revision: &str) -> String {
    let transition = format!("{method} {path}\n{revision}\n{body}");
    format!(
        "anubis-subscription-{}",
        crate::auth::token::hash(&transition)
    )
}

/// One visit to the customer portal, about to be opened.
#[derive(Debug, Clone)]
pub struct NewPortalSession<'a> {
    /// The Stripe customer whose billing is being managed.
    pub customer_id: &'a str,
    /// Where the portal's "return" link sends the browser.
    pub return_url: &'a str,
}

/// A Stripe customer, as much of one as the framework reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Customer {
    /// Stripe's id for the customer, e.g. `cus_1Q...`.
    pub id: String,
}

/// A Stripe Checkout session, as much of one as the framework reads.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutSession {
    /// Stripe's id for the session, e.g. `cs_test_...`.
    pub id: String,
    /// Where the browser has to go to pay.
    pub url: String,
}

/// A Stripe Billing portal session, as much of one as the framework reads.
#[derive(Debug, Clone, Deserialize)]
pub struct PortalSession {
    /// Stripe's id for the session, e.g. `bps_...`.
    pub id: String,
    /// Where the browser has to go to manage the subscription.
    pub url: String,
}

/// The checkout session as it arrives on `checkout.session.completed`.
///
/// A second reading of the same Stripe object as [`CheckoutSession`], because
/// the two readings want different halves of it: opening a session cares about
/// the URL to send a browser to, and a completed one cares about what was
/// bought and for whom.
#[derive(Debug, Clone, Deserialize)]
pub struct CompletedCheckout {
    /// Stripe's id for the session, e.g. `cs_test_...`.
    pub id: String,
    /// The subscription the purchase created, absent for a one-off payment.
    #[serde(default)]
    pub subscription: Option<String>,
    /// The customer that paid, e.g. `cus_1Q...`.
    #[serde(default)]
    pub customer: Option<String>,
    /// What [`NewCheckoutSession`] put on the session: the organization and plan.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// A Stripe subscription, as much of one as the framework reads.
///
/// The fields are read through the accessors below rather than directly,
/// because Stripe has moved some of them between API versions and the accessor
/// is where that is absorbed.
#[derive(Debug, Clone, Deserialize)]
pub struct Subscription {
    /// Stripe's id for the subscription, e.g. `sub_1Q...`.
    pub id: String,
    /// Stripe's status vocabulary: `active`, `past_due`, `canceled`, and so on.
    pub status: String,
    /// The customer being billed, e.g. `cus_1Q...`.
    pub customer: String,
    /// Whether the subscription stops at the end of the paid-for period.
    #[serde(default)]
    pub cancel_at_period_end: bool,
    /// End of the paid-for period, on API versions that carry it here.
    #[serde(default)]
    current_period_end: Option<i64>,
    /// What `subscription_data[metadata]` carried through the checkout.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// The line items, one per price.
    #[serde(default)]
    items: SubscriptionItems,
}

impl Subscription {
    /// Every line item, in the order Stripe lists them.
    pub fn items(&self) -> impl Iterator<Item = &SubscriptionItem> {
        self.items.data.iter()
    }

    /// Whether [`Subscription::items`] is the whole list rather than its first page.
    ///
    /// [`Client`] completes a partial list before handing a subscription over,
    /// so this is false only for one read out of an event body that nothing
    /// has completed yet.
    #[must_use]
    pub fn items_complete(&self) -> bool {
        !self.items.has_more
    }

    /// The line item billing `price_id`, when the subscription has one.
    ///
    /// A subscription bills each price on one item at most, which Stripe
    /// enforces, so a price names an item.
    #[must_use]
    pub fn item_for_price(&self, price_id: &str) -> Option<&SubscriptionItem> {
        self.items().find(|item| item.price_id() == Some(price_id))
    }

    /// The recurrence its prices share, as Stripe spells it: `month` or `year`.
    ///
    /// Stripe requires every item of a subscription to recur on the same
    /// interval, so the first item answers for all of them.
    #[must_use]
    pub fn recurring_interval(&self) -> Option<&str> {
        self.items().find_map(SubscriptionItem::recurring_interval)
    }

    /// When the paid-for period ends, as Unix seconds.
    ///
    /// Stripe moved this field from the subscription onto its items in the
    /// 2025 API versions, and the framework pins no version, so both places are
    /// read and whichever the account's version fills in wins.
    #[must_use]
    pub fn period_end(&self) -> Option<i64> {
        self.current_period_end
            .or_else(|| self.items().find_map(|item| item.current_period_end))
    }

    /// The value Stripe's metadata holds under `name`.
    #[must_use]
    pub fn metadata(&self, name: &str) -> Option<&str> {
        self.metadata.get(name).map(String::as_str)
    }
}

/// The line items of a subscription, as Stripe embeds them.
#[derive(Debug, Clone, Default, Deserialize)]
struct SubscriptionItems {
    #[serde(default)]
    data: Vec<SubscriptionItem>,
    /// Whether Stripe sent only the first page of a longer list.
    #[serde(default)]
    has_more: bool,
}

/// One line item: its id, a price, how many of it, and when its period ends.
#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionItem {
    /// Stripe's id for the item, e.g. `si_1Q...`, which an update names.
    #[serde(default)]
    id: String,
    #[serde(default)]
    price: Option<ItemPrice>,
    #[serde(default)]
    quantity: Option<i64>,
    #[serde(default)]
    current_period_end: Option<i64>,
}

impl SubscriptionItem {
    /// Stripe's id for the item, e.g. `si_1Q...`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The Stripe price the item bills, when Stripe named one.
    #[must_use]
    pub fn price_id(&self) -> Option<&str> {
        self.price.as_ref().map(|price| price.id.as_str())
    }

    /// How many of the price; `None` for a metered price, which bills usage.
    #[must_use]
    pub fn quantity(&self) -> Option<i64> {
        self.quantity
    }

    /// The price's recurrence as Stripe spells it, `month` or `year`.
    #[must_use]
    pub fn recurring_interval(&self) -> Option<&str> {
        self.price
            .as_ref()
            .and_then(|price| price.recurring.as_ref())
            .map(|recurring| recurring.interval.as_str())
    }
}

/// A price on a line item, as much of one as the framework reads.
#[derive(Debug, Clone, Deserialize)]
struct ItemPrice {
    id: String,
    #[serde(default)]
    recurring: Option<Recurring>,
}

/// How often a price recurs, in Stripe's own words.
#[derive(Debug, Clone, Deserialize)]
struct Recurring {
    interval: String,
}

/// One page of a Stripe list response.
#[derive(Debug, Clone, Deserialize)]
struct List<T> {
    data: Vec<T>,
    /// Whether another page follows this one.
    #[serde(default)]
    has_more: bool,
}

/// Stripe's error envelope, the one shape every refusal arrives in.
#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    message: Option<String>,
    code: Option<String>,
}

/// Percent-encodes one path segment.
///
/// Stripe's ids are alphanumeric with underscores, so in practice this returns
/// what it was given. It exists because the ids reaching [`Client`] come out of
/// an event body, and a path is never a place to paste unescaped input.
fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            // The unreserved set of RFC 3986, which needs no encoding anywhere.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => {
                write!(encoded, "%{other:02X}").expect("writing to a String cannot fail");
            }
        }
    }
    encoded
}

/// Builds an `application/x-www-form-urlencoded` body, Stripe's request format.
///
/// A type of its own rather than a bare vector so the encoding lives in one
/// place and call sites read as a list of parameters.
#[derive(Debug, Default)]
struct Form {
    pairs: Vec<(String, String)>,
}

impl Form {
    fn new() -> Self {
        Self::default()
    }

    fn text(&mut self, name: &str, value: &str) {
        self.pairs.push((name.to_owned(), value.to_owned()));
    }

    fn finish(&self) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&self.pairs)
            .finish()
    }
}

/// What went wrong talking to Stripe.
#[derive(Debug)]
enum ErrorKind {
    /// The request never got an answer: DNS, TLS, a timeout, a refusal.
    Transport,
    /// Stripe answered, and said no.
    Api {
        status: u16,
        /// Stripe's own error code, when it named one, e.g. `resource_missing`.
        code: Option<String>,
    },
    /// Stripe answered with something this client could not read.
    Decode,
}

/// A Stripe call that did not succeed.
///
/// The message is Stripe's own where there is one, because it is written for
/// the developer integrating and says exactly what was wrong with the request.
/// It never carries the secret key: the key travels in a header, and no part of
/// a header reaches this type.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// Returns `true` when the request never reached Stripe.
    ///
    /// Worth retrying; an API refusal usually is not.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Transport)
    }

    /// Returns `true` when Stripe answered and refused.
    #[must_use]
    pub fn is_api(&self) -> bool {
        matches!(self.kind, ErrorKind::Api { .. })
    }

    /// The HTTP status Stripe refused with, when it answered at all.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self.kind {
            ErrorKind::Api { status, .. } => Some(status),
            ErrorKind::Transport | ErrorKind::Decode => None,
        }
    }

    /// Stripe's own error code, when it named one, e.g. `resource_missing`.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::Api { code, .. } => code.as_deref(),
            ErrorKind::Transport | ErrorKind::Decode => None,
        }
    }

    fn transport(source: &reqwest::Error) -> Self {
        Self::new(
            ErrorKind::Transport,
            &format!("Stripe is unreachable: {source}"),
        )
    }

    /// Reads Stripe's error envelope, falling back to the raw body.
    fn api(status: u16, body: &str, path: &str) -> Self {
        let parsed: Option<ApiErrorEnvelope> = serde_json::from_str(body).ok();
        let (message, code) = match parsed {
            Some(envelope) => (envelope.error.message, envelope.error.code),
            None => (Some(body.to_owned()), None),
        };
        let message = message.unwrap_or_else(|| "Stripe gave no reason".to_owned());

        Self::new(
            ErrorKind::Api { status, code },
            &format!("Stripe answered {status} for {path}: {message}"),
        )
    }

    fn decode(path: &str, reason: &str) -> Self {
        Self::new(
            ErrorKind::Decode,
            &format!("Stripe's answer to {path} could not be read: {reason}"),
        )
    }

    fn new(kind: ErrorKind, message: &str) -> Self {
        Self {
            kind,
            // Capped on a character boundary: an unexpected body can be as long
            // as whatever proxy produced it.
            message: message.chars().take(MAX_MESSAGE_LENGTH).collect(),
            backtrace: Backtrace::capture(),
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
    use super::{
        API_BASE, Client, Error, Form, Subscription, encode_path_segment, idempotency_key,
    };
    use crate::config::AppConfig;

    /// A key shaped like a real one, belonging to nobody.
    const TEST_KEY: &str = "sk_test_0000000000000000000000000000";

    fn client() -> Client {
        let config = AppConfig::from_lookup(|name| match name {
            "STRIPE_SECRET_KEY" => Some(TEST_KEY.to_owned()),
            _other => None,
        })
        .expect("the test config must parse");
        Client::new(&config.stripe.expect("stripe must be configured"))
    }

    #[test]
    fn the_client_never_renders_its_secret_key() {
        let rendered = format!("{:?}", client());

        assert!(rendered.contains(API_BASE), "got: {rendered}");
        assert!(!rendered.contains("sk_test"), "got: {rendered}");
    }

    #[test]
    fn forms_encode_stripes_nested_parameter_syntax() {
        let mut form = Form::new();
        form.text("line_items[0][price]", "price_pro_monthly");
        form.text(
            "success_url",
            "https://app.example.com/account/billing?ok=1",
        );

        let encoded = form.finish();

        assert!(
            encoded.contains("line_items%5B0%5D%5Bprice%5D=price_pro_monthly"),
            "got: {encoded}",
        );
        assert!(
            encoded.contains("success_url=https%3A%2F%2F"),
            "got: {encoded}"
        );
        // Reading the body twice must not empty it.
        assert_eq!(encoded, form.finish());
    }

    #[test]
    fn a_refusal_carries_stripes_own_message_and_code() {
        let body = r#"{"error":{"message":"No such price: 'price_nope'",
                       "type":"invalid_request_error","code":"resource_missing"}}"#;

        let error = Error::api(400, body, "/v1/checkout/sessions");

        assert!(error.is_api());
        assert!(!error.is_transport());
        assert_eq!(error.status(), Some(400));
        assert_eq!(error.code(), Some("resource_missing"));
        assert!(error.to_string().contains("price_nope"), "got: {error}");
    }

    #[test]
    fn a_subscription_reads_its_items_and_period_from_either_shape() {
        // The 2025 shape: the period lives on the line items.
        let on_the_item: Subscription = serde_json::from_str(
            r#"{
                "id": "sub_1", "object": "subscription", "status": "active",
                "customer": "cus_1", "cancel_at_period_end": false,
                "metadata": { "organization_id": "9f4a", "plan_key": "pro" },
                "items": { "object": "list", "has_more": false, "data": [
                    { "id": "si_1", "quantity": 3, "current_period_end": 1760000000,
                      "price": { "id": "price_pro_monthly", "recurring": { "interval": "month" } } },
                    { "id": "si_2", "current_period_end": 1760000000,
                      "price": { "id": "price_egress", "recurring": { "interval": "month" } } }
                ] }
            }"#,
        )
        .expect("Stripe's subscription must parse");

        let seats = on_the_item
            .item_for_price("price_pro_monthly")
            .expect("the plan's item is found by its price");
        assert_eq!(seats.id(), "si_1");
        assert_eq!(seats.quantity(), Some(3));
        let metered = on_the_item
            .item_for_price("price_egress")
            .expect("an item that is not first is found too");
        assert_eq!(metered.quantity(), None, "a metered price has no quantity");
        assert!(on_the_item.item_for_price("price_nope").is_none());
        assert_eq!(on_the_item.items().count(), 2);
        assert!(on_the_item.items_complete());
        assert_eq!(on_the_item.recurring_interval(), Some("month"));
        assert_eq!(on_the_item.period_end(), Some(1_760_000_000));
        assert_eq!(on_the_item.metadata("plan_key"), Some("pro"));
        assert_eq!(on_the_item.metadata("nothing"), None);

        // The older shape: the period lives on the subscription, the list says
        // more items follow, and fields the framework does not read are absent.
        let on_the_subscription: Subscription = serde_json::from_str(
            r#"{
                "id": "sub_2", "status": "canceled", "customer": "cus_2",
                "current_period_end": 1750000000,
                "items": { "has_more": true, "data": [{ "price": { "id": "price_pro_yearly" } }] }
            }"#,
        )
        .expect("the older shape must parse too");

        assert_eq!(on_the_subscription.period_end(), Some(1_750_000_000));
        assert_eq!(on_the_subscription.recurring_interval(), None);
        assert!(
            !on_the_subscription.items_complete(),
            "a first page is not the whole list",
        );
    }

    #[test]
    fn an_update_is_keyed_by_its_revision_as_well_as_its_body() {
        let path = "/v1/subscriptions/sub_1";
        let body = "items%5B0%5D%5Bprice%5D=price_backups&items%5B0%5D%5Bquantity%5D=1";

        let first = idempotency_key("POST", path, body, "2026-11-24T10:00:00Z");
        let retried = idempotency_key("POST", path, body, "2026-11-24T10:00:00Z");
        let later = idempotency_key("POST", path, body, "2026-11-24T11:30:00Z");

        assert_eq!(
            first, retried,
            "a retry of one change is one change at Stripe"
        );
        assert_ne!(
            first, later,
            "the same change asked for after the row moved on is a new one",
        );
        assert_ne!(
            first,
            idempotency_key(
                "POST",
                path,
                "cancel_at_period_end=true",
                "2026-11-24T10:00:00Z"
            ),
        );
        assert!(first.starts_with("anubis-subscription-"));
        assert!(first.len() <= 255, "Stripe caps a key at 255 characters");
    }

    #[test]
    fn a_path_segment_carries_no_id_of_its_own_making() {
        assert_eq!(encode_path_segment("sub_1QpbQS"), "sub_1QpbQS");
        assert_eq!(
            encode_path_segment("../customers/cus_1"),
            "..%2Fcustomers%2Fcus_1",
            "an id out of an event body cannot climb the path",
        );
        assert_eq!(encode_path_segment("a b"), "a%20b");
    }

    #[test]
    fn a_refusal_that_is_not_stripes_envelope_keeps_the_body() {
        let error = Error::api(502, "<html>gateway</html>", "/v1/customers");

        assert_eq!(error.status(), Some(502));
        assert_eq!(error.code(), None);
        assert!(error.to_string().contains("gateway"), "got: {error}");
    }
}
