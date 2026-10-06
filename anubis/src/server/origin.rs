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
//! Only the address, and only under the rule the rate limiter follows:
//!
//! - **The address** is the last entry of `TRUSTED_PROXY_HEADER` when that is
//!   configured and usable, the socket peer otherwise, through the same
//!   function the limiter reads. The address an event records is therefore the
//!   address its request was charged to.
//! - **The user agent** is the client's own description of itself, and is
//!   recorded as exactly that: a hint for a person reading a log.
//! - **The reported location** is whatever the client sent in
//!   [`REPORTED_LOCATION_HEADER`], and is recorded as exactly that too. A
//!   browser asks a geolocation service where its own address is and passes
//!   the answer along, which costs the deployment no load balancer and no
//!   geolocation database. Anything can send any value, so the name says who
//!   is speaking: the column is `reported_location`, never `location`.
//!
//! Neither the user agent nor the reported location is an input to a decision,
//! anywhere in the framework. A geolocation guess is wrong often enough that a
//! rule built on one would refuse real people, and one the client wrote itself
//! would be a rule the client chose.
//!
//! # Bounded and printable
//!
//! A header is as long as the client makes it, and these are copied into an
//! append-only table, so both strings are cut to a documented length before
//! they are kept, and control characters are dropped: a tab or an escape in a
//! value somebody reads on a screen would say less than nothing. A value that
//! is not visible ASCII at the protocol level is no value at all.
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

/// The header a browser reports its own approximate location in.
///
/// Untrusted by design: see the module docs. A CORS deployment admits it in
/// preflight, so a cross-origin frontend can send it too.
pub const REPORTED_LOCATION_HEADER: HeaderName = HeaderName::from_static("x-reported-location");

/// The most characters of a user agent an event keeps.
///
/// Real browsers send well under three hundred. The bound exists because the
/// value is the client's to choose and lands in a table nothing prunes.
pub const MAX_USER_AGENT_CHARS: usize = 512;

/// The most characters of a reported location an event keeps.
///
/// A city, its region and its country fit in a fraction of this; the bound is
/// for a client that sends something else.
pub const MAX_LOCATION_CHARS: usize = 120;

/// Where one request came from, as far as the deployment can tell.
///
/// Every field is optional, because each depends on something the request may
/// not have: a connection the accept loop described, a browser that names
/// itself, a browser that looked itself up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientOrigin {
    /// The client's address, resolved under the trusted proxy rule.
    pub address: Option<IpAddr>,
    /// The client's description of itself, bounded to [`MAX_USER_AGENT_CHARS`].
    pub user_agent: Option<String>,
    /// Where the client says it is, from [`REPORTED_LOCATION_HEADER`], bounded
    /// to [`MAX_LOCATION_CHARS`]. Display only; never trusted.
    pub reported_location: Option<String>,
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
            reported_location: bounded_header(
                headers,
                &REPORTED_LOCATION_HEADER,
                MAX_LOCATION_CHARS,
            ),
        }
    }
}

/// Which forwarding header a deployment trusts to name the client's address.
///
/// Built from the config [`layer`] is given; see the module docs for why it is
/// trusted only when it is named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginPolicy {
    /// `TRUSTED_PROXY_HEADER`, the forwarding header the client address is in.
    pub proxy_header: Option<HeaderName>,
}

impl OriginPolicy {
    /// The policy a deployment configured.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            proxy_header: config.rate_limit.trusted_proxy_header.clone(),
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

/// One header's value, printable, trimmed, and cut to `limit` characters.
///
/// A value that is not text, or is empty once its control characters are gone
/// and it is trimmed, is no value: a log that printed bytes nobody can read
/// would say less than one that printed nothing.
fn bounded_header(headers: &HeaderMap, name: &HeaderName, limit: usize) -> Option<String> {
    let printable: String = headers
        .get(name)?
        .to_str()
        .ok()?
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let value = printable.trim();
    if value.is_empty() {
        return None;
    }

    Some(value.chars().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    use super::{
        ClientOrigin, MAX_LOCATION_CHARS, MAX_USER_AGENT_CHARS, OriginPolicy,
        REPORTED_LOCATION_HEADER,
    };

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
    fn no_forwarded_address_is_trusted_until_the_deployment_names_a_proxy() {
        let sent = headers(&[("x-forwarded-for", "10.0.0.1")]);

        let origin = ClientOrigin::read(&sent, Some(peer()), &OriginPolicy::default());

        assert_eq!(
            origin.address,
            Some(peer()),
            "the peer is the client until a proxy is trusted"
        );
    }

    #[test]
    fn a_trusted_proxy_and_the_browser_describe_the_client() {
        let policy = OriginPolicy {
            proxy_header: Some(HeaderName::from_static("x-forwarded-for")),
        };
        let sent = headers(&[
            ("x-forwarded-for", "1.2.3.4, 203.0.113.5"),
            ("x-reported-location", " Meridian, Idaho, US "),
            ("user-agent", "Mozilla/5.0 (X11; Linux x86_64)"),
        ]);

        let origin = ClientOrigin::read(&sent, Some(peer()), &policy);

        assert_eq!(
            origin.address,
            Some("203.0.113.5".parse().expect("a literal address parses")),
            "the hop the proxy appended, never the one the client wrote first",
        );
        assert_eq!(
            origin.reported_location.as_deref(),
            Some("Meridian, Idaho, US")
        );
        assert_eq!(
            origin.user_agent.as_deref(),
            Some("Mozilla/5.0 (X11; Linux x86_64)")
        );
    }

    #[test]
    fn a_reported_location_needs_no_configuration_and_is_absent_when_not_sent() {
        let sent = headers(&[("x-reported-location", "Meridian, Idaho, US")]);
        let origin = ClientOrigin::read(&sent, None, &OriginPolicy::default());
        assert_eq!(
            origin.reported_location.as_deref(),
            Some("Meridian, Idaho, US")
        );

        let silent = ClientOrigin::read(&HeaderMap::new(), None, &OriginPolicy::default());
        assert_eq!(silent.reported_location, None);
    }

    #[test]
    fn a_reported_location_is_cut_to_its_bound_and_stripped_of_control_characters() {
        let long = "a".repeat(MAX_LOCATION_CHARS * 2);
        let origin = ClientOrigin::read(
            &headers(&[("x-reported-location", &long)]),
            None,
            &OriginPolicy::default(),
        );
        assert_eq!(
            origin
                .reported_location
                .map(|location| location.chars().count()),
            Some(MAX_LOCATION_CHARS),
        );

        // A tab is the one control character a header value can carry.
        let tabbed = ClientOrigin::read(
            &headers(&[("x-reported-location", "Meridian,\tIdaho")]),
            None,
            &OriginPolicy::default(),
        );
        assert_eq!(tabbed.reported_location.as_deref(), Some("Meridian,Idaho"));

        let only_tabs = ClientOrigin::read(
            &headers(&[("x-reported-location", "\t\t")]),
            None,
            &OriginPolicy::default(),
        );
        assert_eq!(only_tabs.reported_location, None);
    }

    #[test]
    fn a_reported_location_that_is_not_visible_ascii_is_no_value() {
        let mut sent = HeaderMap::new();
        sent.insert(
            REPORTED_LOCATION_HEADER,
            HeaderValue::from_bytes("Zürich".as_bytes()).expect("opaque bytes are a value"),
        );

        let origin = ClientOrigin::read(&sent, None, &OriginPolicy::default());

        assert_eq!(origin.reported_location, None);
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
