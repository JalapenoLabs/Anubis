//! The response security headers and the cross-origin posture.
//!
//! # Security headers
//!
//! Every response carries the same four headers, on by default because a
//! header nobody has to remember to add is the only kind that is always there:
//!
//! | Header | Value | What it buys |
//! |---|---|---|
//! | `X-Content-Type-Options` | `nosniff` | A browser never second-guesses a `Content-Type`, so an uploaded file cannot be coaxed into executing as script |
//! | `Referrer-Policy` | `strict-origin-when-cross-origin` | A path carrying an invitation or reset token never leaves in a `Referer` to another site |
//! | `X-Frame-Options` | `DENY` | No framing, so clickjacking has nothing to hang a transparent overlay on |
//! | `Content-Security-Policy` | `frame-ancestors 'none'` | The same rule in the header that superseded `X-Frame-Options`; both ship, because browsers still disagree about which they honor |
//!
//! The CSP is deliberately one directive. A real content policy for the SPA
//! needs `script-src` and `style-src` tied to the hashes or nonces of a
//! particular Vite build, which is a build-pipeline change rather than a header
//! change: the server would have to learn what the bundler emitted. Shipping a
//! guessed `default-src` instead would either break the application or be so
//! permissive it proves nothing. `frame-ancestors` is the part that is honest
//! today, and the rest is tracked as follow-up work.
//!
//! ## HSTS
//!
//! `Strict-Transport-Security` is sent only in production, and only when the
//! request arrived over https or `APP_URL` is an https URL. Both conditions
//! matter. A browser that receives HSTS for `localhost` refuses plain http to
//! `localhost` for a year, across every project on that machine, and the only
//! cure is clearing browser state by hand: that is a development environment
//! broken by a production header. In production, either the deployment already
//! terminates TLS (so `APP_URL` says https) or a proxy tells us the hop it
//! accepted (`x-forwarded-proto`), and the header does its job of removing the
//! first plain-http request from every later visit.
//!
//! A client can of course claim `x-forwarded-proto: https` itself. All that
//! buys it is an HSTS header on its own response, which browsers ignore unless
//! the connection really was https, so the header is not worth guarding.
//!
//! # CORS
//!
//! The default is no CORS headers at all, which is the strictest posture a
//! browser understands: a cross-origin read is refused without the server
//! saying anything. `CORS_ALLOWED_ORIGINS` opts in by naming exact origins,
//! comma-separated, validated at startup.
//!
//! Credentials are never allowed. The cross-origin consumer this exists for is
//! the public `/api/v1` surface, which authenticates with a bearer token the
//! caller attaches deliberately; session cookies stay same-origin, where the
//! `SameSite=Lax` cookie already keeps them. That also removes the classic
//! footgun in one stroke: there is no combination of settings here that pairs
//! credentials with a permissive origin, because there are no credentials and
//! no wildcard.

use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::uri::Scheme;
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::AppConfig;

/// Never guess a response's type from its bytes.
const NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");

/// Send the full URL to our own origin, the origin alone to https elsewhere,
/// and nothing at all when leaving https for http.
const REFERRER_POLICY: HeaderValue = HeaderValue::from_static("strict-origin-when-cross-origin");

/// No framing at all, by anyone.
const DENY: HeaderValue = HeaderValue::from_static("DENY");

/// The modern spelling of the same rule; see the module docs on scope.
const FRAME_ANCESTORS: HeaderValue = HeaderValue::from_static("frame-ancestors 'none'");

/// A year of https, subdomains included.
///
/// A year is the industry floor and what the preload list asks for. Preload
/// itself is deliberately absent: submitting a domain to it is close to
/// irreversible and is the operator's decision, not the framework's.
const HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains");

/// The header a TLS-terminating proxy names its inbound scheme in.
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// How long a browser may reuse one preflight result.
///
/// Ten minutes spares the preflight on a burst of calls while keeping a policy
/// change effective within the same deploy window someone is watching.
const PREFLIGHT_MAX_AGE: Duration = Duration::from_mins(10);

/// Which origins may call this application from a browser.
///
/// Built by [`crate::config::AppConfig`] from `CORS_ALLOWED_ORIGINS`, which is
/// where the parsing and validation live. Empty, the default, means no CORS
/// headers are sent at all; see the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorsConfig {
    /// Exact origins, normalized, such as `https://app.example.com`.
    pub allowed_origins: Vec<String>,
}

/// Adds the security headers to every response the router produces.
pub(crate) fn secure(router: Router, config: &AppConfig) -> Router {
    router.layer(from_fn_with_state(Policy::new(config), apply))
}

/// Adds the CORS layer, when the environment named origins to allow.
pub(crate) fn allow_cross_origin(router: Router, config: &AppConfig) -> Router {
    match cors_layer(&config.cors) {
        Some(layer) => router.layer(layer),
        None => router,
    }
}

/// Whether HSTS applies, decided once at startup and checked per request.
#[derive(Debug, Clone, Copy)]
struct Policy {
    /// HSTS is a production-only header; see the module docs.
    production: bool,
    /// Whether the public base URL the application advertises is https.
    public_url_is_https: bool,
}

impl Policy {
    fn new(config: &AppConfig) -> Self {
        Self {
            production: config.environment.is_production(),
            public_url_is_https: config.app_url.starts_with("https://"),
        }
    }

    /// Returns whether this request's response should carry HSTS.
    fn sends_hsts(self, request: &Request) -> bool {
        self.production && (self.public_url_is_https || arrived_over_https(request))
    }
}

/// Stamps the policy onto one response.
async fn apply(State(policy): State<Policy>, request: Request, next: Next) -> Response {
    let hsts = policy.sends_hsts(&request);

    let mut response = next.run(request).await;

    // Insert rather than append: the policy is the framework's to decide, and
    // a duplicate header is a policy two parties disagree about.
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, NOSNIFF);
    headers.insert(header::REFERRER_POLICY, REFERRER_POLICY);
    headers.insert(header::X_FRAME_OPTIONS, DENY);
    headers.insert(header::CONTENT_SECURITY_POLICY, FRAME_ANCESTORS);
    if hsts {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HSTS);
    }

    response
}

/// Returns whether the request reached the deployment over https.
///
/// The first entry of `x-forwarded-proto` is the scheme the client used, the
/// entries after it being later hops. Direct https never reaches this server,
/// which speaks plain http and leaves TLS to whatever fronts it, so the URI
/// check only matters when something else rewrites the request.
fn arrived_over_https(request: &Request) -> bool {
    if request.uri().scheme() == Some(&Scheme::HTTPS) {
        return true;
    }

    request
        .headers()
        .get(X_FORWARDED_PROTO)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|scheme| scheme.trim().eq_ignore_ascii_case("https"))
}

/// Builds the CORS layer, or `None` when no origin is allowed.
fn cors_layer(config: &CorsConfig) -> Option<CorsLayer> {
    if config.allowed_origins.is_empty() {
        return None;
    }

    let origins: Vec<HeaderValue> = config
        .allowed_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin)
                .expect("configuration accepts only origins that are valid header values")
        })
        .collect();

    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            // Everything a JSON API call needs and nothing else: a bearer
            // token and a content type.
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .max_age(PREFLIGHT_MAX_AGE),
    )
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::Request;

    use super::{CorsConfig, Policy, arrived_over_https, cors_layer};

    fn request(forwarded_proto: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/api/v1/team");
        if let Some(scheme) = forwarded_proto {
            builder = builder.header("x-forwarded-proto", scheme);
        }
        builder.body(Body::empty()).expect("must build")
    }

    fn policy(production: bool, public_url_is_https: bool) -> Policy {
        Policy {
            production,
            public_url_is_https,
        }
    }

    #[test]
    fn the_client_scheme_is_the_first_forwarded_entry() {
        assert!(arrived_over_https(&request(Some("https"))));
        assert!(arrived_over_https(&request(Some("HTTPS, http"))));
        assert!(!arrived_over_https(&request(Some("http, https"))));
        assert!(!arrived_over_https(&request(Some("nonsense"))));
        assert!(!arrived_over_https(&request(None)));
    }

    #[test]
    fn hsts_stays_out_of_development_however_the_request_arrived() {
        let development = policy(false, true);

        assert!(!development.sends_hsts(&request(Some("https"))));
        assert!(!development.sends_hsts(&request(None)));
    }

    #[test]
    fn production_sends_hsts_once_https_is_in_the_picture() {
        let behind_tls = policy(true, true);
        assert!(behind_tls.sends_hsts(&request(None)));

        // An http APP_URL still gets HSTS on a request a proxy took over https.
        let plain = policy(true, false);
        assert!(plain.sends_hsts(&request(Some("https"))));
        assert!(!plain.sends_hsts(&request(None)));
    }

    #[test]
    fn no_allowed_origin_means_no_cors_layer_at_all() {
        assert!(cors_layer(&CorsConfig::default()).is_none());

        let configured = CorsConfig {
            allowed_origins: vec!["https://app.example.com".to_owned()],
        };
        assert!(cors_layer(&configured).is_some());
    }
}
