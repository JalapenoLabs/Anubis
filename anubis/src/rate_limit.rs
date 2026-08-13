//! Per-client request limits for abuse-prone endpoints.
//!
//! Individual tokens are attempt-limited, but the endpoints that accept them
//! are not: without this module a client may spend passwords, sign-in codes,
//! and reset emails at network speed. [`RateLimiter`] gives each client a
//! balance per [`Budget`] and answers `429 Too Many Requests` with a
//! `Retry-After` header once it is spent.
//!
//! Routes opt in through [`RateLimiter::layer`], a [`tower::Layer`] applied to
//! one route at a time, so the budget a route carries is visible where the
//! route is declared:
//!
//! ```ignore
//! Router::new()
//!     .route("/login", post(login).layer(limiter.layer(Budget::Credentials)))
//!     .route("/register", post(register).layer(limiter.layer(Budget::Registration)))
//! ```
//!
//! Budgets keyed by something inside the request body, today only
//! [`Budget::EmailPerRecipient`], cannot be charged by a layer that has not
//! read the body, so handlers charge those themselves with
//! [`RateLimiter::check_recipient`].
//!
//! # The algorithm
//!
//! Each key is one [`Instant`], its theoretical arrival time: the moment its
//! balance is full again. This is GCRA, the generic cell rate algorithm, also
//! known as a leaky bucket used as a meter. A request is admitted when
//! advancing that instant by one emission interval (`period / quota`) leaves
//! it no further than `period` ahead of now, which permits a burst of `quota`
//! requests and then one request per emission interval. The rejection carries
//! exactly how long the caller must wait, and unlike a fixed window it cannot
//! be gamed by straddling a boundary to spend two windows back to back.
//!
//! # Bounded memory
//!
//! State is in-process and bounded to [`MAX_TRACKED_KEYS`] keys, roughly a
//! hundred bytes each, so the map costs a few megabytes at worst. Keys are
//! added only by requests that are being limited, and the map is pruned when
//! it reaches the bound: first of keys whose balance is already full, which
//! carry no information, then of the least-loaded keys that remain. A client
//! rotating addresses to flood the map therefore evicts its own fresh keys
//! long before the keys the limiter is actively holding back, because those
//! are the heaviest in the map.
//!
//! # Scope
//!
//! Limits are per process. Two instances behind a load balancer enforce two
//! budgets, so the effective limit multiplies by the instance count. That
//! still bounds the attack: an attacker cannot exceed `instances * quota` per
//! period, and instance counts are small compared with the unlimited case
//! this replaces. A shared store (Redis) is the eventual answer and is not
//! required to make the endpoints safe today.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Once};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::HeaderName;
use axum::response::{IntoResponse, Response};
use tower::{Layer, Service};

use crate::http::ApiError;

/// Credential attempts one client may spend per [`CREDENTIAL_PERIOD`].
///
/// A human signs in in one to three attempts and a password manager in one,
/// so ten leaves room for typos and for a few colleagues sharing one office
/// address. It also cuts online guessing from network speed to 14,400
/// attempts a day, which is nothing against argon2id password hashing, and
/// less than nothing against a six-digit code that expires in ten minutes and
/// dies after a handful of wrong attempts per token.
const CREDENTIAL_ATTEMPTS: u32 = 10;

/// The window [`CREDENTIAL_ATTEMPTS`] is spent over.
///
/// A minute keeps the wait short for a user who trips the budget: it refills
/// one attempt every six seconds rather than all at once.
const CREDENTIAL_PERIOD: Duration = Duration::from_mins(1);

/// Accounts one client may register per [`REGISTRATION_PERIOD`].
///
/// Signing up is a once-per-person act. Ten an hour still covers a whole team
/// onboarding from one office address, while turning bulk account creation
/// from one address into a trickle.
const REGISTRATIONS: u32 = 10;

/// The window [`REGISTRATIONS`] is spent over.
const REGISTRATION_PERIOD: Duration = Duration::from_hours(1);

/// Emails one client may cause per [`EMAIL_PERIOD`].
///
/// Every message this covers is user-triggered (a sign-in code, a reset link,
/// a re-sent verification) and a user needs one or two per sign-in. Twenty an
/// hour absorbs a shared office address; past that, one client is generating
/// mail faster than anyone reads it.
const EMAILS_PER_CLIENT: u32 = 20;

/// Emails one address may receive per [`EMAIL_PERIOD`].
///
/// Mail bombing aims at an inbox rather than at a client, and attackers
/// rotate client addresses freely, so the tighter budget belongs to the
/// recipient. Five an hour covers a user retrying a sign-in code a few times
/// and caps what any number of clients together can deliver to one inbox.
const EMAILS_PER_RECIPIENT: u32 = 5;

/// The window [`EMAILS_PER_CLIENT`] and [`EMAILS_PER_RECIPIENT`] are spent over.
///
/// An hour matches the timescale of the abuse: mail bombing is a sustained
/// campaign, not a burst.
const EMAIL_PERIOD: Duration = Duration::from_hours(1);

/// How many keys the limiter tracks before it starts evicting.
///
/// At roughly a hundred bytes per key this bounds the map at a few megabytes,
/// which is the point: an attacker cycling client addresses must not be able
/// to grow it without limit. It is also far above the working set of a real
/// deployment, where only clients that are actively being limited appear, so
/// eviction is an attack-time behavior rather than a steady-state one.
pub const MAX_TRACKED_KEYS: usize = 32_768;

/// How many keys survive when the bound is reached under a flood.
///
/// Half the bound, so a flood pays for a full prune rather than one eviction
/// per request.
const KEYS_KEPT_UNDER_PRESSURE: usize = MAX_TRACKED_KEYS / 2;

/// How clients are addressed, and whether limiting is switched on at all.
///
/// Built by [`crate::config::AppConfig`] from `RATE_LIMIT_DISABLED` and
/// `TRUSTED_PROXY_HEADER`; see that module for the environment contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Turns limiting off entirely, for development and test runs.
    pub disabled: bool,
    /// The header a trusted proxy appends the client address to.
    ///
    /// `None` means the socket peer address is the client address, which is
    /// correct whenever the application terminates connections itself.
    pub trusted_proxy_header: Option<HeaderName>,
}

/// What a request is charged against: one quota over one period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Budget {
    /// Credential guessing: login, MFA verification, sign-in code verification.
    Credentials,
    /// Account spam: registration.
    Registration,
    /// Mail bombing measured per client: every endpoint that sends email.
    EmailPerClient,
    /// Mail bombing measured per target address, whoever asks for it.
    EmailPerRecipient,
}

impl Budget {
    /// Returns how many requests a key may spend per [`Budget::period`].
    #[must_use]
    pub fn quota(self) -> u32 {
        match self {
            Self::Credentials => CREDENTIAL_ATTEMPTS,
            Self::Registration => REGISTRATIONS,
            Self::EmailPerClient => EMAILS_PER_CLIENT,
            Self::EmailPerRecipient => EMAILS_PER_RECIPIENT,
        }
    }

    /// Returns the window a key's [`Budget::quota`] is spent over.
    #[must_use]
    pub fn period(self) -> Duration {
        match self {
            Self::Credentials => CREDENTIAL_PERIOD,
            Self::Registration => REGISTRATION_PERIOD,
            Self::EmailPerClient | Self::EmailPerRecipient => EMAIL_PERIOD,
        }
    }

    /// Returns how long a spent key waits to regain one request.
    fn emission_interval(self) -> Duration {
        self.period() / self.quota()
    }

    /// Distinguishes budgets sharing one map, so a client tracked under two
    /// of them keeps two independent balances.
    fn prefix(self) -> &'static str {
        match self {
            Self::Credentials => "credentials",
            Self::Registration => "registration",
            Self::EmailPerClient => "email-client",
            Self::EmailPerRecipient => "email-recipient",
        }
    }
}

/// Tracks what each client has spent, and rejects what exceeds its budget.
///
/// Cloning shares one set of balances, so an application builds a single
/// limiter and hands clones to every router that needs one.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    enabled: bool,
    trusted_proxy_header: Option<HeaderName>,
    balances: Mutex<HashMap<String, Instant>>,
    /// Guards the "no client address" warning, which is a wiring bug that
    /// would otherwise be reported on every single request.
    unaddressed_warning: Once,
}

impl RateLimiter {
    /// Creates a limiter over the given configuration.
    #[must_use]
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                enabled: !config.disabled,
                trusted_proxy_header: config.trusted_proxy_header.clone(),
                balances: Mutex::new(HashMap::new()),
                unaddressed_warning: Once::new(),
            }),
        }
    }

    /// Returns a layer charging every request on a route to `budget`.
    ///
    /// Apply it to a single route rather than to a router, so the budget is
    /// declared next to the route it protects:
    ///
    /// ```ignore
    /// .route("/login", post(login).layer(limiter.layer(Budget::Credentials)))
    /// ```
    #[must_use]
    pub fn layer(&self, budget: Budget) -> RateLimitLayer {
        RateLimitLayer {
            limiter: self.clone(),
            budget,
        }
    }

    /// Charges one request against the per-recipient email budget.
    ///
    /// Handlers that mail an address taken from the request body call this
    /// with that address, after normalizing it and before looking anything
    /// up, so an attacker rotating client addresses to bomb one inbox meets
    /// the same balance from every address. The answer does not depend on
    /// whether the address belongs to an account, so it reveals nothing the
    /// endpoint itself does not.
    ///
    /// The address never becomes a map key; its SHA-256 does.
    ///
    /// # Errors
    /// Returns a `429` [`ApiError`] carrying the wait once the budget is spent.
    pub fn check_recipient(&self, email: &str) -> Result<(), ApiError> {
        self.check(
            Budget::EmailPerRecipient,
            &crate::auth::token::hash(email),
            Instant::now(),
        )
    }

    /// Charges one request against `budget`, keyed by the client address.
    fn check_request(&self, budget: Budget, request: &Request) -> Result<(), ApiError> {
        if !self.inner.enabled {
            return Ok(());
        }

        let Some(address) = self.client_address(request) else {
            // Only a composition bug produces a request with no address: the
            // peer address comes from the accept loop, never from the client.
            // Passing it through keeps a misconfigured deployment serving
            // while the log names the fix; refusing would take the whole
            // sign-in surface down for everyone.
            self.inner.unaddressed_warning.call_once(|| {
                tracing::warn!(
                    "requests carry no client address, so rate limiting is inactive; \
                     serve the router with \
                     `into_make_service_with_connect_info::<SocketAddr>()`",
                );
            });
            return Ok(());
        };

        self.check(budget, &address.to_string(), Instant::now())
    }

    /// Resolves the address a request is charged to.
    ///
    /// A configured proxy header wins when it carries a usable address, and
    /// the socket peer address answers otherwise. A configured header that is
    /// missing or unparsable therefore falls back to the peer, which is the
    /// proxy: limits get stricter, never looser, when a proxy is
    /// misconfigured.
    fn client_address(&self, request: &Request) -> Option<IpAddr> {
        if let Some(header) = &self.inner.trusted_proxy_header
            && let Some(forwarded) = forwarded_address(request, header)
        {
            return Some(forwarded);
        }

        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(peer)| peer.ip())
    }

    /// Charges one request against `budget` for `principal`, as of `now`.
    ///
    /// Taking the clock as an argument keeps the accounting testable without
    /// sleeping through a period.
    fn check(&self, budget: Budget, principal: &str, now: Instant) -> Result<(), ApiError> {
        if !self.inner.enabled {
            return Ok(());
        }

        let key = format!("{}:{principal}", budget.prefix());
        let mut balances = self
            .inner
            .balances
            .lock()
            .expect("the rate limit map is poisoned");

        // GCRA: `arrival` is when this key's balance is full again. Spending
        // one request pushes it out by one emission interval; the request is
        // admitted while that stays within one period of now, which is a
        // burst of `quota` followed by one request per emission interval.
        let arrival = balances.get(&key).copied().unwrap_or(now).max(now);
        let spent = arrival + budget.emission_interval();
        let ahead = spent.saturating_duration_since(now);
        if ahead > budget.period() {
            return Err(ApiError::too_many_requests(
                ahead.saturating_sub(budget.period()),
            ));
        }

        if balances.insert(key, spent).is_none() {
            enforce_bound(&mut balances, now);
        }
        Ok(())
    }
}

/// Reads the client address a trusted proxy appended to `header`.
///
/// Only the last entry of the last occurrence counts. A forwarding header is
/// a trail of hops, and every entry before the last was either written by an
/// upstream the application does not control or sent by the client itself, so
/// trusting the first entry would let any caller pick its own rate limit key.
/// The last entry is the one the trusted proxy appended, which is the address
/// it accepted the connection from.
fn forwarded_address(request: &Request, header: &HeaderName) -> Option<IpAddr> {
    let value = request
        .headers()
        .get_all(header)
        .iter()
        .next_back()?
        .to_str()
        .ok()?;

    value.rsplit(',').next()?.trim().parse().ok()
}

/// Keeps the map inside [`MAX_TRACKED_KEYS`], dropping the least-loaded keys.
fn enforce_bound(balances: &mut HashMap<String, Instant>, now: Instant) {
    if balances.len() <= MAX_TRACKED_KEYS {
        return;
    }

    // A key whose arrival time has passed has its whole balance back, so
    // forgetting it changes no decision the limiter would make.
    balances.retain(|_key, arrival| *arrival > now);
    if balances.len() <= MAX_TRACKED_KEYS {
        return;
    }

    // Every key still owes something, so this is a flood of distinct keys.
    // Keep the heaviest spenders, whose arrival times sit furthest in the
    // future: the flood's own keys are the lightest in the map, so it evicts
    // itself rather than the clients the limiter is holding back.
    let mut arrivals: Vec<Instant> = balances.values().copied().collect();
    arrivals.sort_unstable();
    let threshold = arrivals[arrivals.len() - KEYS_KEPT_UNDER_PRESSURE];

    // Everything above the threshold survives outright. Keys sitting exactly
    // on it fill whatever room is left, in whatever order the map yields
    // them: that is the only arbitrary choice here, and it is only ever made
    // between keys that owe the same amount. A flood of keys that all arrived
    // at once is exactly that case, which is why the comparison alone would
    // not be enough to protect the heaviest key.
    let above = arrivals
        .iter()
        .filter(|arrival| **arrival > threshold)
        .count();
    let mut room_at_threshold = KEYS_KEPT_UNDER_PRESSURE.saturating_sub(above);
    balances.retain(|_key, arrival| {
        if *arrival > threshold {
            return true;
        }
        let fits = *arrival == threshold && room_at_threshold > 0;
        room_at_threshold -= usize::from(fits);
        fits
    });
}

/// Charges every request on a route to one [`Budget`].
///
/// Built by [`RateLimiter::layer`].
#[derive(Debug, Clone)]
pub struct RateLimitLayer {
    limiter: RateLimiter,
    budget: Budget,
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimited<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimited {
            inner,
            limiter: self.limiter.clone(),
            budget: self.budget,
        }
    }
}

/// The service [`RateLimitLayer`] wraps a route in.
#[derive(Debug, Clone)]
pub struct RateLimited<S> {
    inner: S,
    limiter: RateLimiter,
    budget: Budget,
}

impl<S> Service<Request> for RateLimited<S>
where
    S: Service<Request, Response = Response>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    // The decision itself is synchronous; boxing here only unifies the two
    // outcomes, and one allocation is noise beside the password hashing and
    // database round trips on the routes this protects.
    type Future = Pin<Box<dyn Future<Output = Result<Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if let Err(error) = self.limiter.check_request(self.budget, &req) {
            return Box::pin(std::future::ready(Ok(error.into_response())));
        }

        Box::pin(self.inner.call(req))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    use axum::extract::{ConnectInfo, Request};
    use axum::http::{HeaderName, StatusCode};

    use super::{Budget, KEYS_KEPT_UNDER_PRESSURE, MAX_TRACKED_KEYS, RateLimitConfig, RateLimiter};

    fn limiter() -> RateLimiter {
        RateLimiter::new(&RateLimitConfig::default())
    }

    /// A request carrying a peer address, and optionally a forwarding header.
    fn request(peer: &str, forwarded: Option<(&str, &str)>) -> Request {
        let mut builder = Request::builder().uri("/login");
        if let Some((header, value)) = forwarded {
            builder = builder.header(header, value);
        }
        let mut request = builder.body(axum::body::Body::empty()).expect("must build");
        let peer: SocketAddr = peer.parse().expect("peer must parse");
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    #[test]
    fn a_key_bursts_its_quota_then_refills_one_request_at_a_time() {
        let limiter = limiter();
        let budget = Budget::Credentials;
        let start = Instant::now();

        for attempt in 0..budget.quota() {
            limiter
                .check(budget, "198.51.100.7", start)
                .unwrap_or_else(|_error| panic!("attempt {attempt} is within the burst"));
        }

        let error = limiter
            .check(budget, "198.51.100.7", start)
            .expect_err("the burst is spent");
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);

        // Not yet: the balance refills one request per emission interval.
        let almost = start
            + budget
                .emission_interval()
                .saturating_sub(Duration::from_millis(1));
        limiter
            .check(budget, "198.51.100.7", almost)
            .expect_err("the interval has not elapsed");

        let refilled = start + budget.emission_interval();
        limiter
            .check(budget, "198.51.100.7", refilled)
            .expect("one request is back");
        limiter
            .check(budget, "198.51.100.7", refilled)
            .expect_err("only one request is back");

        // A full period away, the whole burst is available again.
        let rolled = start + budget.period() * 2;
        for attempt in 0..budget.quota() {
            limiter
                .check(budget, "198.51.100.7", rolled)
                .unwrap_or_else(|_error| panic!("attempt {attempt} is within the fresh burst"));
        }
    }

    #[test]
    fn the_rejection_says_how_long_to_wait() {
        let limiter = limiter();
        let budget = Budget::Credentials;
        let start = Instant::now();

        for _attempt in 0..budget.quota() {
            limiter
                .check(budget, "198.51.100.8", start)
                .expect("within the burst");
        }

        let error = limiter
            .check(budget, "198.51.100.8", start)
            .expect_err("the burst is spent");
        assert_eq!(
            error.retry_after(),
            Some(budget.emission_interval()),
            "the wait is one emission interval",
        );
    }

    #[test]
    fn keys_and_budgets_are_accounted_separately() {
        let limiter = limiter();
        let budget = Budget::Credentials;
        let now = Instant::now();

        for _attempt in 0..budget.quota() {
            limiter
                .check(budget, "198.51.100.9", now)
                .expect("within the burst");
        }
        limiter
            .check(budget, "198.51.100.9", now)
            .expect_err("the burst is spent");

        limiter
            .check(budget, "203.0.113.9", now)
            .expect("another client is untouched");
        limiter
            .check(Budget::Registration, "198.51.100.9", now)
            .expect("another budget is untouched");
    }

    #[test]
    fn the_map_stays_bounded_and_evicts_the_lightest_keys() {
        let limiter = limiter();
        let budget = Budget::Credentials;
        let now = Instant::now();

        // A heavy key: its whole burst is spent, so it owes the most.
        for _attempt in 0..budget.quota() {
            limiter
                .check(budget, "heavy", now)
                .expect("within the burst");
        }

        // A flood of distinct keys, each spending one request.
        for index in 0..=MAX_TRACKED_KEYS {
            limiter
                .check(budget, &format!("flood-{index}"), now)
                .expect("a fresh key always has budget");
        }

        let tracked = limiter
            .inner
            .balances
            .lock()
            .expect("the map is not poisoned")
            .len();
        assert!(
            tracked <= MAX_TRACKED_KEYS,
            "the map must stay bounded, got {tracked}",
        );
        assert!(
            tracked >= KEYS_KEPT_UNDER_PRESSURE,
            "pruning must not empty the map, got {tracked}",
        );

        limiter
            .check(budget, "heavy", now)
            .expect_err("the flood must not buy the heavy key a fresh balance");
    }

    #[test]
    fn a_disabled_limiter_admits_everything() {
        let limiter = RateLimiter::new(&RateLimitConfig {
            disabled: true,
            trusted_proxy_header: None,
        });
        let now = Instant::now();

        for _attempt in 0..Budget::Credentials.quota() * 10 {
            limiter
                .check(Budget::Credentials, "198.51.100.10", now)
                .expect("a disabled limiter never rejects");
        }
        limiter
            .check_recipient("someone@example.com")
            .expect("a disabled limiter never rejects");
    }

    #[test]
    fn the_peer_address_is_the_key_until_a_proxy_header_is_trusted() {
        let limiter = limiter();
        let spoofed = request("198.51.100.11:54321", Some(("x-forwarded-for", "10.0.0.1")));

        assert_eq!(
            limiter.client_address(&spoofed),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 11))),
            "an untrusted header must not choose the key",
        );
    }

    #[test]
    fn a_trusted_header_yields_the_hop_the_proxy_appended() {
        let limiter = RateLimiter::new(&RateLimitConfig {
            disabled: false,
            trusted_proxy_header: Some(HeaderName::from_static("x-forwarded-for")),
        });

        // The client sent "1.2.3.4"; the proxy appended what it saw.
        let forwarded = request(
            "10.0.0.1:8080",
            Some(("x-forwarded-for", "1.2.3.4, 203.0.113.5")),
        );
        assert_eq!(
            limiter.client_address(&forwarded),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5))),
            "the last hop is the only trustworthy entry",
        );

        // Nothing usable in the header: the peer answers instead.
        for value in ["", "not-an-address", "1.2.3.4, bogus"] {
            let broken = request("10.0.0.1:8080", Some(("x-forwarded-for", value)));
            assert_eq!(
                limiter.client_address(&broken),
                Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                "for header value {value:?}",
            );
        }

        let absent = request("10.0.0.1:8080", None);
        assert_eq!(
            limiter.client_address(&absent),
            Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
        );
    }

    #[test]
    fn a_request_without_an_address_is_admitted() {
        let limiter = limiter();
        let request = Request::builder()
            .uri("/login")
            .body(axum::body::Body::empty())
            .expect("must build");

        assert_eq!(limiter.client_address(&request), None);
        for _attempt in 0..Budget::Credentials.quota() * 3 {
            limiter
                .check_request(Budget::Credentials, &request)
                .expect("an unaddressable request is never charged");
        }
    }

    #[test]
    fn recipients_are_hashed_and_limited_per_address() {
        let limiter = limiter();

        for _attempt in 0..Budget::EmailPerRecipient.quota() {
            limiter
                .check_recipient("victim@example.com")
                .expect("within the burst");
        }
        limiter
            .check_recipient("victim@example.com")
            .expect_err("the inbox budget is spent");
        limiter
            .check_recipient("someone-else@example.com")
            .expect("another inbox is untouched");

        let balances = limiter
            .inner
            .balances
            .lock()
            .expect("the map is not poisoned");
        assert!(
            balances.keys().all(|key| !key.contains("victim")),
            "addresses must not sit in the map in the clear",
        );
    }
}
