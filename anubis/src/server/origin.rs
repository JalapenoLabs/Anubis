//! Where a request came from: the client's address, browser, and location.
//!
//! [`layer`] reads the three once per request and leaves them on it as a
//! [`ClientOrigin`] extension, the way the request id is left. The audit log is
//! the reader: [`crate::audit::Context`] picks the extension up, so every event
//! a request records says where the act came from, and a sign-in is the one
//! that needs it most.
//!
//! # What is trusted
//!
//! Only what a proxy the deployment controls wrote, and nothing a client could
//! choose:
//!
//! - **The address** follows the rule the rate limiter follows, through the
//!   same function: the last entry of `TRUSTED_PROXY_HEADER` when that is
//!   configured and usable, the socket peer otherwise. The address an event
//!   records is therefore the address its request was charged to.
//! - **The location** is read only from `TRUSTED_LOCATION_HEADER`, and only
//!   when that is configured. A load balancer that overwrites the header on
//!   every request makes it the balancer's guess at the client's region and
//!   city; anywhere else it is whatever the client typed, which is why an
//!   unset variable records none rather than reading a conventional name.
//! - **The user agent** is the client's own description of itself, and is
//!   recorded as exactly that: a hint for a person reading a log, never an
//!   input to a decision.
//!
//! Location in particular is display only. Nothing in the framework decides
//! anything on it, because a geolocation guess is wrong often enough that a
//! rule built on one would refuse real people.
//!
//! # Bounded
//!
//! A header is as long as the client makes it, and these are copied into an
//! append-only table, so both strings are cut to a documented length before
//! they are kept.
//!
//! # Outside a served request
//!
//! A router that is not wrapped in [`crate::server::harden`] carries no
//! extension, and an event recorded from it says nothing about origin. That
//! is the same contract the request id has, and it is the truth: nothing
//! measured where that request came from.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, HeaderName, Response};
use axum::middleware::Next;

use crate::config::AppConfig;

/// The most characters of a user agent an event keeps.
///
/// Real browsers send well under three hundred. The bound exists because the
/// value is the client's to choose and lands in a table nothing prunes.
pub const MAX_USER_AGENT_CHARS: usize = 512;

/// The most characters of a location an event keeps.
///
/// A load balancer's region and city fit in a fraction of this; the bound is
/// for a misconfigured header that carries something else.
pub const MAX_LOCATION_CHARS: usize = 120;

/// Where one request came from, as far as the deployment can tell.
///
/// Every field is optional, because each depends on something the request or
/// the deployment may not have: a connection the accept loop described, a
/// browser that names itself, a load balancer that geolocates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientOrigin {
    /// The client's address, resolved under the trusted proxy rule.
    pub address: Option<IpAddr>,
    /// The client's description of itself, bounded to [`MAX_USER_AGENT_CHARS`].
    pub user_agent: Option<String>,
    /// The trusted load balancer's guess at the client's location, bounded to
    /// [`MAX_LOCATION_CHARS`]. Absent unless `TRUSTED_LOCATION_HEADER` is set.
    pub location: Option<String>,
}

impl ClientOrigin {
    /// Reads a request's origin under `policy`.
    #[must_use]
    pub fn read(headers: &HeaderMap, peer: Option<IpAddr>, policy: &OriginPolicy) -> Self {
        let forwarded = policy
            .proxy_header
            .as_ref()
            .and_then(|header| crate::rate_limit::forwarded_address(headers, header));

        Self {
            address: forwarded.or(peer),
            user_agent: bounded_header(headers, &USER_AGENT, MAX_USER_AGENT_CHARS),
            location: policy
                .location_header
                .as_ref()
                .and_then(|header| bounded_header(headers, header, MAX_LOCATION_CHARS)),
        }
    }
}

/// Which headers a deployment trusts to describe a request's origin.
///
/// Built from the config [`layer`] is given; see the module docs for why each
/// one is trusted only when it is named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginPolicy {
    /// `TRUSTED_PROXY_HEADER`, the forwarding header the client address is in.
    pub proxy_header: Option<HeaderName>,
    /// `TRUSTED_LOCATION_HEADER`, the header the client's location is in.
    pub location_header: Option<HeaderName>,
}

impl OriginPolicy {
    /// The policy a deployment configured.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            proxy_header: config.rate_limit.trusted_proxy_header.clone(),
            location_header: config.trusted_location_header.clone(),
        }
    }
}

/// Records each request's [`ClientOrigin`] as an extension on it.
///
/// [`crate::server::harden`] applies it to every application. It is public on
/// its own so a test can apply the one layer without the whole stack.
pub fn layer(router: axum::Router, config: &AppConfig) -> axum::Router {
    router.layer(axum::middleware::from_fn_with_state(
        OriginPolicy::from_config(config),
        record_origin,
    ))
}

async fn record_origin(
    State(policy): State<OriginPolicy>,
    mut request: Request,
    next: Next,
) -> Response<axum::body::Body> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip());
    let origin = ClientOrigin::read(request.headers(), peer, &policy);
    request.extensions_mut().insert(origin);
    next.run(request).await
}

/// One header's value, trimmed and cut to `limit` characters.
///
/// A value that is not text, or is empty once trimmed, is no value: a log
/// that printed bytes nobody can read would say less than one that printed
/// nothing.
fn bounded_header(headers: &HeaderMap, name: &HeaderName, limit: usize) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }

    Some(value.chars().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    use super::{ClientOrigin, MAX_USER_AGENT_CHARS, OriginPolicy};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).expect("test header values are valid"),
            );
        }
        map
    }

    fn peer() -> IpAddr {
        "198.51.100.7".parse().expect("a literal address parses")
    }

    #[test]
    fn nothing_a_client_sends_is_trusted_until_the_deployment_names_it() {
        let sent = headers(&[
            ("x-forwarded-for", "10.0.0.1"),
            ("x-client-geo-location", "US,mountain view"),
        ]);

        let origin = ClientOrigin::read(&sent, Some(peer()), &OriginPolicy::default());

        assert_eq!(
            origin.address,
            Some(peer()),
            "the peer is the client until a proxy is trusted"
        );
        assert_eq!(
            origin.location, None,
            "a location nobody vouched for is not recorded"
        );
    }

    #[test]
    fn a_trusted_proxy_and_balancer_describe_the_client() {
        let policy = OriginPolicy {
            proxy_header: Some(HeaderName::from_static("x-forwarded-for")),
            location_header: Some(HeaderName::from_static("x-client-geo-location")),
        };
        let sent = headers(&[
            ("x-forwarded-for", "1.2.3.4, 203.0.113.5"),
            ("x-client-geo-location", " US,mountain view "),
            ("user-agent", "Mozilla/5.0 (X11; Linux x86_64)"),
        ]);

        let origin = ClientOrigin::read(&sent, Some(peer()), &policy);

        assert_eq!(
            origin.address,
            Some("203.0.113.5".parse().expect("a literal address parses")),
            "the hop the proxy appended, never the one the client wrote first",
        );
        assert_eq!(origin.location.as_deref(), Some("US,mountain view"));
        assert_eq!(
            origin.user_agent.as_deref(),
            Some("Mozilla/5.0 (X11; Linux x86_64)")
        );
    }

    #[test]
    fn a_header_is_cut_to_its_bound_and_an_empty_one_is_absent() {
        let long = "a".repeat(MAX_USER_AGENT_CHARS * 2);
        let origin = ClientOrigin::read(
            &headers(&[("user-agent", &long)]),
            None,
            &OriginPolicy::default(),
        );
        assert_eq!(
            origin.user_agent.map(|agent| agent.chars().count()),
            Some(MAX_USER_AGENT_CHARS),
        );

        let blank = ClientOrigin::read(
            &headers(&[("user-agent", "   ")]),
            None,
            &OriginPolicy::default(),
        );
        assert_eq!(blank.user_agent, None);
    }
}
