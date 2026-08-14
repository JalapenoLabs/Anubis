//! The handful of Stripe API calls the billing module makes.
//!
//! Anubis talks to Stripe over its REST API directly, with [`Client`], rather
//! than through a generated SDK. The reasoning is in `docs/billing.md`, and it
//! comes down to surface area: the whole integration is three calls today and
//! five when subscription events land, every one of them a form-encoded POST
//! answering flat JSON. The framework already carries a `reqwest` client built
//! on rustls for outgoing webhooks, so this module adds no dependency, no
//! second TLS stack, and no compile time worth measuring.
//!
//! # What is here
//!
//! | Call | Stripe endpoint | Used by |
//! |---|---|---|
//! | [`Client::create_customer`] | `POST /v1/customers` | The first checkout an organization starts |
//! | [`Client::create_checkout_session`] | `POST /v1/checkout/sessions` | Starting a subscription |
//! | [`Client::create_portal_session`] | `POST /v1/billing_portal/sessions` | Managing an existing one |
//! | [`Client::retrieve_subscription`] | `GET /v1/subscriptions/{id}` | Reading current state when an event arrives |
//! | [`Client::list_subscriptions`] | `GET /v1/subscriptions` | Reconciling an organization against Stripe |
//!
//! The two reads exist because an event's body is a snapshot of the moment it
//! was created, and a job may process it minutes later. Stripe's own guidance
//! is to treat a payload as possibly stale and read the object back, which is
//! what [`crate::billing::Reconciler`] does; see `docs/billing.md`.
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
//! one customer instead of racing to create two.
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

/// How many of a customer's subscriptions one reconciliation reads.
///
/// Stripe's own maximum for a list page. An organization that has held more
/// subscriptions than this has a decade of history and the oldest of them are
/// long over, so a second page would correct nothing; taking one page keeps
/// reconciliation a single call.
const MAX_SUBSCRIPTIONS_LISTED: u8 = 100;

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
        let idempotency_key = format!("anubis-customer-{}", customer.organization_id);
        self.post("/v1/customers", &form, Some(&idempotency_key))
            .await
    }

    /// Opens a Stripe Checkout session for one subscription purchase.
    ///
    /// The session carries the organization and plan in its metadata, and on
    /// the subscription it creates, so the events that follow identify the
    /// tenant without a lookup table of Stripe ids.
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
        form.text("line_items[0][price]", session.price_id);
        form.text("line_items[0][quantity]", &session.quantity.to_string());
        form.text("success_url", session.success_url);
        form.text("cancel_url", session.cancel_url);
        // Stripe echoes this on the completed-session event; the metadata below
        // rides the subscription itself, which is what every later event names.
        form.text("client_reference_id", &organization_id);
        form.text("metadata[organization_id]", &organization_id);
        form.text("metadata[plan_key]", session.plan_key);
        form.text(
            "subscription_data[metadata][organization_id]",
            &organization_id,
        );
        form.text("subscription_data[metadata][plan_key]", session.plan_key);

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
        self.get(&path, &[]).await
    }

    /// Lists every subscription a customer has ever held, newest first.
    ///
    /// `status=all` is deliberate: reconciliation has to see the cancelled ones
    /// too, or a row this application still believes is live would have nothing
    /// to correct it.
    ///
    /// # Errors
    /// Returns an [`Error`] when Stripe refuses the call or cannot be reached.
    pub async fn list_subscriptions(&self, customer_id: &str) -> Result<Vec<Subscription>, Error> {
        let list: List<Subscription> = self
            .get(
                "/v1/subscriptions",
                &[
                    ("customer", customer_id),
                    ("status", "all"),
                    ("limit", &MAX_SUBSCRIPTIONS_LISTED.to_string()),
                ],
            )
            .await?;

        Ok(list.data)
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
            request = request.header("Idempotency-Key", key);
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

/// One subscription purchase, about to be handed to Stripe Checkout.
#[derive(Debug, Clone)]
pub struct NewCheckoutSession<'a> {
    /// The Stripe customer paying, e.g. `cus_1Q...`.
    pub customer_id: &'a str,
    /// The Stripe price being bought, from `config/billing.yml`.
    pub price_id: &'a str,
    /// Seats bought. One for flat pricing.
    pub quantity: i64,
    /// Where Stripe returns the browser after a completed purchase.
    pub success_url: &'a str,
    /// Where Stripe returns the browser when the customer backs out.
    pub cancel_url: &'a str,
    /// The organization being subscribed, carried into Stripe's metadata.
    pub organization_id: Uuid,
    /// The plan being bought, carried into Stripe's metadata.
    pub plan_key: &'a str,
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
    /// The line items, one per price. The framework bills a single price.
    #[serde(default)]
    items: SubscriptionItems,
}

impl Subscription {
    /// The Stripe price this subscription buys, which names the plan.
    ///
    /// `None` for a subscription with no line items, which Stripe does not
    /// produce but a mock or a future API version might.
    #[must_use]
    pub fn price_id(&self) -> Option<&str> {
        self.first_item()
            .and_then(|item| item.price.as_ref())
            .map(|price| price.id.as_str())
    }

    /// The price's recurrence as Stripe spells it, `month` or `year`.
    ///
    /// Read only when a price is not one `config/billing.yml` names, which is
    /// the one case where the interval cannot come from the plan.
    #[must_use]
    pub fn recurring_interval(&self) -> Option<&str> {
        self.first_item()
            .and_then(|item| item.price.as_ref())
            .and_then(|price| price.recurring.as_ref())
            .map(|recurring| recurring.interval.as_str())
    }

    /// Seats bought, which is the line item's quantity.
    #[must_use]
    pub fn quantity(&self) -> i64 {
        self.first_item()
            .and_then(|item| item.quantity)
            .unwrap_or(1)
    }

    /// When the paid-for period ends, as Unix seconds.
    ///
    /// Stripe moved this field from the subscription onto its items in the
    /// 2025 API versions, and the framework pins no version, so both places are
    /// read and whichever the account's version fills in wins.
    #[must_use]
    pub fn period_end(&self) -> Option<i64> {
        self.current_period_end
            .or_else(|| self.first_item().and_then(|item| item.current_period_end))
    }

    /// The value Stripe's metadata holds under `name`.
    #[must_use]
    pub fn metadata(&self, name: &str) -> Option<&str> {
        self.metadata.get(name).map(String::as_str)
    }

    fn first_item(&self) -> Option<&SubscriptionItem> {
        self.items.data.first()
    }
}

/// The line items of a subscription.
#[derive(Debug, Clone, Default, Deserialize)]
struct SubscriptionItems {
    #[serde(default)]
    data: Vec<SubscriptionItem>,
}

/// One line item: a price, how many of it, and when its period ends.
#[derive(Debug, Clone, Deserialize)]
struct SubscriptionItem {
    #[serde(default)]
    price: Option<ItemPrice>,
    #[serde(default)]
    quantity: Option<i64>,
    #[serde(default)]
    current_period_end: Option<i64>,
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
    use super::{API_BASE, Client, Error, Form, Subscription, encode_path_segment};
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
    fn a_subscription_reads_its_price_quantity_and_period_from_either_shape() {
        // The 2025 shape: the period lives on the line item.
        let on_the_item: Subscription = serde_json::from_str(
            r#"{
                "id": "sub_1", "object": "subscription", "status": "active",
                "customer": "cus_1", "cancel_at_period_end": false,
                "metadata": { "organization_id": "9f4a", "plan_key": "pro" },
                "items": { "object": "list", "data": [{
                    "id": "si_1", "quantity": 3, "current_period_end": 1760000000,
                    "price": { "id": "price_pro_monthly", "recurring": { "interval": "month" } }
                }] }
            }"#,
        )
        .expect("Stripe's subscription must parse");

        assert_eq!(on_the_item.price_id(), Some("price_pro_monthly"));
        assert_eq!(on_the_item.recurring_interval(), Some("month"));
        assert_eq!(on_the_item.quantity(), 3);
        assert_eq!(on_the_item.period_end(), Some(1_760_000_000));
        assert_eq!(on_the_item.metadata("plan_key"), Some("pro"));
        assert_eq!(on_the_item.metadata("nothing"), None);

        // The older shape: the period lives on the subscription, and fields the
        // framework does not read are simply absent.
        let on_the_subscription: Subscription = serde_json::from_str(
            r#"{
                "id": "sub_2", "status": "canceled", "customer": "cus_2",
                "current_period_end": 1750000000,
                "items": { "data": [{ "price": { "id": "price_pro_yearly" } }] }
            }"#,
        )
        .expect("the older shape must parse too");

        assert_eq!(on_the_subscription.period_end(), Some(1_750_000_000));
        assert_eq!(on_the_subscription.recurring_interval(), None);
        assert_eq!(
            on_the_subscription.quantity(),
            1,
            "a line item without a quantity is one seat",
        );
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
