//! Fetching a Client ID Metadata Document, without becoming an SSRF primitive.
//!
//! A client that identifies itself with an https URL (Claude Code's is
//! `https://claude.ai/oauth/claude-code-client-metadata`) is asking this server
//! to fetch that URL and believe what it says about the client. The URL comes
//! from an unauthenticated query string, so the fetch is the most dangerous
//! request this server makes, and every rule below is from
//! `draft-ietf-oauth-client-id-metadata-document`:
//!
//! - The URL is `https`, has a path, carries no fragment, no credentials, and
//!   no dot segments, and is already in its canonical form.
//! - The host is resolved here, every address it resolves to must be a public
//!   one, and the connection is pinned to the address that was checked, so a
//!   DNS answer that changes between the check and the connect cannot point
//!   the fetch inside the network.
//! - Redirects are not followed, only a `200` is a document, and at most five
//!   kilobytes are read.
//! - The document's `client_id` must be the URL exactly, it must name a
//!   client and at least one redirect URI this server would register, and it
//!   must not describe a client that authenticates with a shared secret.
//!
//! The draft lets a deployment for development or testing fetch from loopback
//! addresses. Outside production this module does, over `http` too, which is
//! what lets a test serve a document from `127.0.0.1`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::CACHE_CONTROL;
use serde::Deserialize;
use url::Url;

use crate::oauth_server::redirect;

/// How long a fetch may take, resolving the host and reading the body included.
///
/// Claude's own discovery and registration calls time out at ten seconds, and
/// this fetch happens inside the authorization request it is waiting on. The
/// budget covers the DNS lookup as well as the request, because the host is
/// the client's to choose and a resolver that never answers is the cheapest
/// way to hold a request open.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The most of a document this server reads, the draft's recommendation.
const MAX_DOCUMENT_BYTES: usize = 5 * 1024;

/// How long a document is trusted when its response names no lifetime.
const DEFAULT_CACHE: Duration = Duration::from_hours(1);

/// The shortest a document is cached, whatever its headers say, so a client
/// sending `no-cache` cannot make every authorization request a fetch.
const MIN_CACHE: Duration = Duration::from_mins(5);

/// The longest a document is cached, so a client that changes its redirect
/// URIs is believed within a day.
const MAX_CACHE: Duration = Duration::from_hours(24);

/// The longest client name the consent screen shows.
pub(crate) const MAX_CLIENT_NAME_LENGTH: usize = 100;

/// The most redirect URIs one client may name.
pub(crate) const MAX_REDIRECT_URIS: usize = 10;

/// What a document said about its client, once it passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientDocument {
    pub(crate) name: String,
    pub(crate) client_uri: Option<String>,
    pub(crate) redirect_uris: Vec<String>,
    /// When the document should be fetched again.
    pub(crate) refresh_after: DateTime<Utc>,
}

/// Fetches and checks metadata documents.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fetcher {
    /// Whether loopback hosts and plain `http` are allowed, outside production.
    allow_loopback: bool,
}

impl Fetcher {
    pub(crate) fn new(allow_loopback: bool) -> Self {
        Self { allow_loopback }
    }

    /// Whether `client_id` is a URL this server would fetch as a document.
    ///
    /// A dynamically registered client's id is random base64url, which never
    /// holds a `:`, so the two kinds of id cannot be confused.
    pub(crate) fn is_document_url(self, client_id: &str) -> bool {
        self.check_url(client_id).is_ok()
    }

    /// Fetches the document `client_id` names and checks it, within
    /// [`FETCH_TIMEOUT`] from the first lookup to the last byte.
    ///
    /// # Errors
    /// Returns a sentence naming what was wrong, for the log and for the
    /// consent screen's refusal. Nothing it says comes from the document's
    /// contents, so a hostile document cannot write on this server's pages.
    pub(crate) async fn fetch(self, client_id: &str) -> Result<ClientDocument, &'static str> {
        tokio::time::timeout(FETCH_TIMEOUT, self.fetch_unbounded(client_id))
            .await
            .map_err(|_elapsed| "the client's metadata document took longer than five seconds")?
    }

    /// The fetch itself, which [`Fetcher::fetch`] bounds in time.
    async fn fetch_unbounded(self, client_id: &str) -> Result<ClientDocument, &'static str> {
        let url = self.check_url(client_id)?;
        let host = url.host_str().ok_or("the client id names no host")?;
        let port = url
            .port_or_known_default()
            .ok_or("the client id names no port")?;
        let addresses = self.resolve(host, port).await?;

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            // The connection goes to the addresses checked above and nowhere
            // else, whatever the resolver would answer by the time it opens.
            // All of them, in the resolver's order, so a host answering IPv6
            // first is still reached from a network that routes only IPv4.
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_error| "the metadata client could not be built")?;

        let mut response = http
            .get(url.as_str())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_error| "the client's metadata document could not be fetched")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("the client's metadata document did not answer 200");
        }

        let lifetime = cache_lifetime(response.headers().get(CACHE_CONTROL));
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_error| "the client's metadata document could not be read")?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_DOCUMENT_BYTES {
                return Err("the client's metadata document is larger than five kilobytes");
            }
        }

        let document: RawDocument = serde_json::from_slice(&body)
            .map_err(|_error| "the client's metadata document is not valid JSON metadata")?;
        let refresh_after =
            Utc::now() + chrono::Duration::from_std(lifetime).unwrap_or(chrono::Duration::hours(1));
        check_document(client_id, document, refresh_after)
    }

    /// The URL rules, which hold before anything is resolved.
    fn check_url(self, client_id: &str) -> Result<Url, &'static str> {
        let url = Url::parse(client_id).map_err(|_error| "the client id is not a URL")?;

        // Parsing normalizes dot segments, default ports, and case, so a URL
        // that changed on the way through was not in canonical form. The
        // draft compares client ids as plain strings, and a non-canonical one
        // would name a different client than the document it fetches.
        if url.as_str() != client_id {
            return Err("the client id is not a canonical URL");
        }
        let scheme_allowed = match url.scheme() {
            "https" => true,
            "http" => self.allow_loopback && redirect::is_loopback(&url),
            _ => false,
        };
        if !scheme_allowed {
            return Err("the client id must be an https URL");
        }
        if url.path() == "/" {
            return Err("the client id must have a path");
        }
        if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err("the client id must not carry a fragment or credentials");
        }
        Ok(url)
    }

    /// Resolves `host` and refuses it unless every address is public.
    ///
    /// Every address, rather than the first, because a host answering one
    /// public and one private address leaves which one is dialed to chance.
    async fn resolve(self, host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        // `host_str` keeps the brackets on an IPv6 literal, which neither the
        // resolver nor the IP parser accepts.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let addresses: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_not_a_literal) => tokio::net::lookup_host((bare, port))
                .await
                .map_err(|_error| "the client id's host does not resolve")?
                .collect(),
        };

        if addresses.is_empty() {
            return Err("the client id's host does not resolve");
        }
        let all_allowed = addresses.iter().all(|address| {
            is_public(address.ip()) || (self.allow_loopback && address.ip().is_loopback())
        });
        if !all_allowed {
            return Err("the client id's host is not a public address");
        }
        Ok(addresses)
    }
}

/// The fields this server reads from a document; everything else is ignored.
#[derive(Deserialize)]
struct RawDocument {
    client_id: String,
    client_name: Option<String>,
    client_uri: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    client_secret: Option<serde_json::Value>,
}

/// The content rules, once the bytes are JSON.
fn check_document(
    client_id: &str,
    document: RawDocument,
    refresh_after: DateTime<Utc>,
) -> Result<ClientDocument, &'static str> {
    if document.client_id != client_id {
        return Err("the client's metadata document names a different client id");
    }
    if document.client_secret.is_some() {
        return Err("the client's metadata document carries a client secret");
    }
    // `none` is the only method this server authenticates a client with. A
    // document leaving it out is read the same way, because this server never
    // issues a secret it could otherwise mean.
    if document
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
    {
        return Err("the client's metadata document asks for client authentication");
    }

    let name = document
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or("the client's metadata document names no client")?;
    if name.chars().count() > MAX_CLIENT_NAME_LENGTH {
        return Err("the client's name is longer than 100 characters");
    }

    if document.redirect_uris.is_empty() || document.redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err("the client's metadata document must name 1 to 10 redirect URIs");
    }
    if document
        .redirect_uris
        .iter()
        .any(|uri| redirect::check_registrable(uri).is_err())
    {
        return Err("the client's metadata document names a redirect URI this server refuses");
    }

    Ok(ClientDocument {
        name: name.to_owned(),
        // A homepage is shown as a link, so only an https one is kept.
        client_uri: document
            .client_uri
            .filter(|uri| uri.starts_with("https://") && Url::parse(uri).is_ok()),
        redirect_uris: document.redirect_uris,
        refresh_after,
    })
}

/// How long a response may be cached, read from its `Cache-Control`.
///
/// `max-age` is honored inside the bounds above; `no-store` and `no-cache`
/// read as the minimum rather than as zero, so a client cannot force a fetch
/// per authorization request.
fn cache_lifetime(header: Option<&reqwest::header::HeaderValue>) -> Duration {
    let Some(value) = header.and_then(|value| value.to_str().ok()) else {
        return DEFAULT_CACHE;
    };

    let mut lifetime = DEFAULT_CACHE;
    for directive in value.split(',').map(str::trim) {
        if directive.eq_ignore_ascii_case("no-store") || directive.eq_ignore_ascii_case("no-cache")
        {
            return MIN_CACHE;
        }
        if let Some(seconds) = directive
            .strip_prefix("max-age=")
            .and_then(|seconds| seconds.parse::<u64>().ok())
        {
            lifetime = Duration::from_secs(seconds);
        }
    }
    lifetime.clamp(MIN_CACHE, MAX_CACHE)
}

/// Whether an address is one the public internet routes to.
///
/// Everything RFC 6890 sets aside is refused: private, loopback, link-local,
/// shared, documentation, benchmarking, reserved, multicast, and the IPv6
/// translation prefixes that would smuggle one of those back in.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    let special = ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        // 0.0.0.0/8, "this network".
        || first == 0
        // 100.64.0.0/10, carrier-grade NAT.
        || (first == 100 && (64..=127).contains(&second))
        // 192.0.0.0/24, IETF protocol assignments.
        || (first == 192 && second == 0 && third == 0)
        // 198.18.0.0/15, benchmarking.
        || (first == 198 && (second == 18 || second == 19))
        // 240.0.0.0/4, reserved.
        || first >= 240;
    !special
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_v4(mapped);
    }

    let segments = ip.segments();
    // Global unicast is 2000::/3; everything outside it is special-use.
    let global_unicast = (segments[0] & 0xE000) == 0x2000;
    // 2001:db8::/32 is documentation, 2001::/32 is Teredo, 2002::/16 is 6to4.
    let special =
        (segments[0] == 0x2001 && matches!(segments[1], 0x0DB8 | 0x0000)) || segments[0] == 0x2002;
    global_unicast && !special
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::time::Duration;

    use chrono::Utc;
    use reqwest::header::HeaderValue;

    use super::{
        DEFAULT_CACHE, FETCH_TIMEOUT, Fetcher, MAX_CACHE, MIN_CACHE, RawDocument, cache_lifetime,
        check_document, is_public,
    };

    const CLAUDE_CODE: &str = "https://claude.ai/oauth/claude-code-client-metadata";

    fn document(client_id: &str) -> RawDocument {
        RawDocument {
            client_id: client_id.to_owned(),
            client_name: Some("Claude Code".to_owned()),
            client_uri: Some("https://claude.ai".to_owned()),
            redirect_uris: vec![
                "http://localhost/callback".to_owned(),
                "http://127.0.0.1/callback".to_owned(),
            ],
            token_endpoint_auth_method: Some("none".to_owned()),
            client_secret: None,
        }
    }

    #[test]
    fn claude_codes_published_document_is_accepted() {
        let checked = check_document(CLAUDE_CODE, document(CLAUDE_CODE), Utc::now())
            .expect("Claude Code's document is a valid public client");

        assert_eq!(checked.name, "Claude Code");
        assert_eq!(checked.redirect_uris.len(), 2);
    }

    #[test]
    fn a_document_for_another_client_id_is_refused() {
        check_document(
            CLAUDE_CODE,
            document("https://evil.example/metadata"),
            Utc::now(),
        )
        .expect_err("a document must name the URL it was fetched from");
    }

    #[test]
    fn a_confidential_client_is_refused() {
        let mut secret = document(CLAUDE_CODE);
        secret.token_endpoint_auth_method = Some("client_secret_basic".to_owned());
        check_document(CLAUDE_CODE, secret, Utc::now()).expect_err("the document must be refused");

        let mut carrying = document(CLAUDE_CODE);
        carrying.client_secret = Some(serde_json::json!("hunter2"));
        check_document(CLAUDE_CODE, carrying, Utc::now())
            .expect_err("the document must be refused");
    }

    #[test]
    fn a_document_naming_an_unregistrable_redirect_is_refused() {
        let mut remote_http = document(CLAUDE_CODE);
        remote_http.redirect_uris = vec!["http://evil.example/callback".to_owned()];

        check_document(CLAUDE_CODE, remote_http, Utc::now())
            .expect_err("the document must be refused");
    }

    #[test]
    fn production_accepts_only_canonical_https_urls_with_a_path() {
        let fetcher = Fetcher::new(false);

        assert!(fetcher.is_document_url(CLAUDE_CODE));
        for refused in [
            "http://claude.ai/oauth/claude-code-client-metadata",
            "https://claude.ai/",
            "https://claude.ai/a/../metadata",
            "https://claude.ai/metadata#fragment",
            "https://user@claude.ai/metadata",
            "HTTPS://claude.ai/metadata",
            "http://127.0.0.1:4000/metadata",
            "a1B2c3",
        ] {
            assert!(!fetcher.is_document_url(refused), "{refused}");
        }
    }

    #[test]
    fn development_also_accepts_loopback_http() {
        assert!(Fetcher::new(true).is_document_url("http://127.0.0.1:4000/metadata"));
    }

    #[test]
    fn special_use_addresses_are_not_public() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:10.0.0.1",
            "2001:db8::1",
        ] {
            let ip: IpAddr = address.parse().expect("the test address parses");
            assert!(!is_public(ip), "{address}");
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let ip: IpAddr = address.parse().expect("the test address parses");
            assert!(is_public(ip), "{address}");
        }
    }

    /// The budget is the fetch's own, not the HTTP client's, so it holds a
    /// host that accepts the connection and never answers.
    #[tokio::test]
    async fn a_host_that_never_answers_is_refused_within_the_budget() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port must be available");
        let port = listener.local_addr().expect("the listener is bound").port();
        let started = std::time::Instant::now();

        let refused = Fetcher::new(true)
            .fetch(&format!("http://127.0.0.1:{port}/client.json"))
            .await;

        assert_eq!(
            refused,
            Err("the client's metadata document took longer than five seconds"),
        );
        assert!(started.elapsed() < FETCH_TIMEOUT + Duration::from_secs(2));
        drop(listener);
    }

    #[test]
    fn cache_lifetimes_are_read_and_bounded() {
        let header = |value: &'static str| Some(HeaderValue::from_static(value));

        assert_eq!(cache_lifetime(None), DEFAULT_CACHE);
        assert_eq!(
            cache_lifetime(header("public, max-age=7200").as_ref()),
            Duration::from_hours(2),
        );
        assert_eq!(cache_lifetime(header("max-age=1").as_ref()), MIN_CACHE);
        assert_eq!(
            cache_lifetime(header("max-age=99999999").as_ref()),
            MAX_CACHE
        );
        assert_eq!(cache_lifetime(header("no-store").as_ref()), MIN_CACHE);
    }
}
