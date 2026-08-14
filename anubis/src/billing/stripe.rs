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
//!
//! Everything else about a subscription arrives as an event rather than being
//! asked for, which is why the client stays this small.
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
use std::fmt::{self, Debug, Display, Formatter};
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
    use super::{API_BASE, Client, Error, Form};
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
    fn a_refusal_that_is_not_stripes_envelope_keeps_the_body() {
        let error = Error::api(502, "<html>gateway</html>", "/v1/customers");

        assert_eq!(error.status(), Some(502));
        assert_eq!(error.code(), None);
        assert!(error.to_string().contains("gateway"), "got: {error}");
    }
}
