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
//! | `Content-Security-Policy` | the policy below | What the page may load, and from where |
//!
//! # The content security policy
//!
//! One policy, rendered once at startup and stamped on every response:
//!
//! ```text
//! default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline';
//! img-src 'self' data: blob:; font-src 'self'; connect-src 'self' wss://app.example.com;
//! base-uri 'self'; form-action 'self'; frame-ancestors 'none'
//! ```
//!
//! It is grounded in what the frontend build actually emits and what the
//! running application actually loads, directive by directive:
//!
//! | Directive | Value | Why |
//! |---|---|---|
//! | `default-src` | `'none'` | The base case is refusal, so a resource type nobody thought about is denied rather than inherited from a permissive default |
//! | `script-src` | `'self'` | The Vite build emits no inline script at all: `index.html` carries one hashed module and one stylesheet, both same-origin, and the lazy chunks are same-origin imports. Nothing in the bundle compiles code at runtime, so no `'unsafe-eval'` either |
//! | `style-src` | `'self' 'unsafe-inline'` | See below |
//! | `img-src` | `'self' data: blob:` | Avatars and the TOTP QR code are served by this binary. The avatar picker previews the chosen file through `URL.createObjectURL`, which is a `blob:` URL. `data:` rides along because it is how a canvas or an inline SVG hands a browser an image, and because it names no origin, so configuration could not add it later |
//! | `font-src` | `'self'` | The build self-hosts every face; nothing reaches a font CDN |
//! | `connect-src` | `'self'` and the websocket origin | Every fetch goes to this server's own API, and the realtime channel is a websocket to it. CSP level 3 has `'self'` cover `ws:` on the same host, but browsers implemented that late and a realtime channel that dies silently in one of them is the worst kind of bug, so the origin of `APP_URL` is spelled out beside it |
//! | `base-uri` | `'self'` | An injected `<base>` repoints every relative URL on the page, the ones the SPA fetches with included |
//! | `form-action` | `'self'` | A form may only post back here. The directive does not fall back to `default-src`, so leaving it out would allow every destination |
//! | `frame-ancestors` | `'none'` | The modern spelling of `X-Frame-Options`; both ship, because browsers still disagree about which they honor |
//!
//! ## Why `'unsafe-inline'` is in `style-src`
//!
//! The component libraries write stylesheets into the document at runtime.
//! React Aria adds a `touch-action` rule for every pressable element and an
//! `overscroll-behavior` rule while a modal holds the scroll, and Motion
//! inserts one to hold a leaving element in place while it animates out. Some
//! of those honor a nonce and some, including a second copy of React Aria's
//! press handling, set none at all. A nonce would therefore leave the
//! unnonced ones broken, which is a policy that reports success while removing
//! behavior, and hashes cannot cover rules whose text is computed from an
//! element's measured position.
//!
//! This is the standard concession, and it is a small one: `style-src` is not
//! a code execution boundary. `script-src 'self'` with no inline scripts and no
//! `'unsafe-eval'` is where the protection lives, and it is intact.
//!
//! ## Extending the policy
//!
//! An application that adds an analytics endpoint, an error ingest, or an
//! image CDN names those sources in `CSP_ALLOWED_SOURCES`, which is
//! directive-qualified and validated at startup:
//!
//! ```sh
//! CSP_ALLOWED_SOURCES="script-src https://plausible.io; connect-src https://plausible.io"
//! ```
//!
//! Only the directives in [`extendable_directive`] take sources, and only
//! sources naming a host. What is deliberately impossible from an environment
//! variable: widening `default-src`, `base-uri`, `form-action`, or
//! `frame-ancestors`, whose whole value is that they name nothing, and adding
//! `'unsafe-eval'` or any other keyword, which would turn a configuration typo
//! into an execution gate.
//!
//! ## The one response that carries its own
//!
//! A handler that sets a `Content-Security-Policy` itself keeps it: the layer
//! fills the header in, it does not overwrite. Exactly one response in the
//! framework does that, the API reference at `/api/v1/docs`, which renders
//! through Scalar from a CDN and would otherwise be a blank page. It carries
//! [`API_REFERENCE_CSP`], which widens what that page loads and keeps
//! everything that protects the application around it.
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

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::uri::Scheme;
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use tower_http::cors::{AllowOrigin, CorsLayer};
use url::Url;

use crate::config::AppConfig;

/// Never guess a response's type from its bytes.
const NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");

/// Send the full URL to our own origin, the origin alone to https elsewhere,
/// and nothing at all when leaving https for http.
const REFERRER_POLICY: HeaderValue = HeaderValue::from_static("strict-origin-when-cross-origin");

/// No framing at all, by anyone.
const DENY: HeaderValue = HeaderValue::from_static("DENY");

/// The framework's policy, directive by directive, in the order it renders.
///
/// Every directive names its own sources, because `default-src 'none'` at the
/// head means one left out inherits nothing rather than everything. The
/// per-directive reasoning is in the module docs. `frame-src` and `media-src`
/// carry no sources of their own and render only when an application adds
/// some, an embedded payment widget or a video host being the cases that need
/// them.
const POLICY: &[(&str, &str)] = &[
    ("default-src", "'none'"),
    ("script-src", "'self'"),
    ("style-src", "'self' 'unsafe-inline'"),
    ("img-src", "'self' data: blob:"),
    ("font-src", "'self'"),
    ("connect-src", "'self'"),
    ("frame-src", ""),
    ("media-src", ""),
    ("base-uri", "'self'"),
    ("form-action", "'self'"),
    ("frame-ancestors", "'none'"),
];

/// The directive the realtime websocket origin joins.
const CONNECT_SRC: &str = "connect-src";

/// The policy the API reference page carries instead of the framework's.
///
/// Scalar renders the OpenAPI document from a CDN bundle that styles itself as
/// it runs, so the application's policy would leave a blank page. This one
/// widens exactly that and keeps everything that protects the deployment
/// around it: no framing, no plugins, no form posting elsewhere, and no
/// `'unsafe-eval'`. The page takes no user input and renders a document this
/// application generated, so what it may load is a smaller question than it is
/// anywhere else.
pub(crate) const API_REFERENCE_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; script-src 'self' https://cdn.jsdelivr.net; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; \
     font-src 'self' data: https:; connect-src 'self'; base-uri 'self'; \
     form-action 'self'; frame-ancestors 'none'",
);

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

/// The sources an application adds to the content security policy.
///
/// Built by [`crate::config::AppConfig`] from `CSP_ALLOWED_SOURCES`, which is
/// where the parsing and validation live. Empty, the default, means the
/// framework's policy is the whole policy; see the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CspConfig {
    /// Extra sources, keyed by the directive they join.
    ///
    /// Keys come from [`extendable_directive`], so they are always a directive
    /// the policy knows how to render.
    pub additional_sources: BTreeMap<&'static str, Vec<String>>,
}

/// Returns the canonical name of a directive configuration may extend.
///
/// The directives left out are the ones whose whole value is that they name
/// nothing: `default-src`, `base-uri`, `form-action`, and `frame-ancestors`.
/// Widening one of those from an environment variable would undo the policy
/// rather than extend it, so it stays a code change with a reviewer attached.
pub(crate) fn extendable_directive(name: &str) -> Option<&'static str> {
    /// The fetch directives an application has a legitimate reason to widen.
    const EXTENDABLE: &[&str] = &[
        "script-src",
        "style-src",
        "img-src",
        "font-src",
        "connect-src",
        "frame-src",
        "media-src",
    ];

    EXTENDABLE
        .iter()
        .copied()
        .find(|directive| directive.eq_ignore_ascii_case(name))
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

/// The response policy, decided once at startup and applied per request.
#[derive(Debug, Clone)]
struct Policy {
    /// HSTS is a production-only header; see the module docs.
    production: bool,
    /// Whether the public base URL the application advertises is https.
    public_url_is_https: bool,
    /// The content security policy, rendered from the configuration.
    csp: HeaderValue,
}

impl Policy {
    fn new(config: &AppConfig) -> Self {
        Self {
            production: config.environment.is_production(),
            public_url_is_https: config.app_url.starts_with("https://"),
            csp: content_security_policy(config),
        }
    }

    /// Returns whether this request's response should carry HSTS.
    fn sends_hsts(&self, request: &Request) -> bool {
        self.production && (self.public_url_is_https || arrived_over_https(request))
    }
}

/// Stamps the policy onto one response.
async fn apply(State(policy): State<Policy>, request: Request, next: Next) -> Response {
    let hsts = policy.sends_hsts(&request);

    let mut response = next.run(request).await;

    // Insert rather than append: these are the framework's to decide, and a
    // duplicate header is a policy two parties disagree about.
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, NOSNIFF);
    headers.insert(header::REFERRER_POLICY, REFERRER_POLICY);
    headers.insert(header::X_FRAME_OPTIONS, DENY);
    // The content policy is the exception: a handler that set one made a
    // narrower decision about its own response, and two policies would be
    // intersected into something neither party intended.
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert_with(|| policy.csp.clone());
    if hsts {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HSTS);
    }

    response
}

/// Renders the content security policy this deployment sends.
///
/// Called once at startup: the policy depends only on configuration, and a
/// header value assembled per request would be the same string every time.
fn content_security_policy(config: &AppConfig) -> HeaderValue {
    let websocket_origin = websocket_origin(&config.app_url);
    let mut rendered = String::new();

    for (directive, base) in POLICY {
        let mut sources: Vec<&str> = base.split_whitespace().collect();
        if *directive == CONNECT_SRC
            && let Some(origin) = websocket_origin.as_deref()
        {
            sources.push(origin);
        }
        if let Some(additional) = config.csp.additional_sources.get(directive) {
            sources.extend(additional.iter().map(String::as_str));
        }
        if sources.is_empty() {
            continue;
        }

        if !rendered.is_empty() {
            rendered.push_str("; ");
        }
        rendered.push_str(directive);
        for source in sources {
            rendered.push(' ');
            rendered.push_str(source);
        }
    }

    HeaderValue::from_str(&rendered)
        .expect("configuration accepts only sources that are valid header values")
}

/// Returns the websocket origin of the application's public URL.
///
/// `http` becomes `ws` and `https` becomes `wss`, which is the origin the
/// browser opens the realtime channel on. `None` when `APP_URL` is neither, in
/// which case `connect-src 'self'` is the whole answer; see the module docs on
/// why the origin is named at all.
fn websocket_origin(app_url: &str) -> Option<String> {
    let url = Url::parse(app_url).ok()?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return None,
    };
    let host = url.host_str()?;

    match url.port() {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
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
    use axum::http::HeaderValue;

    use super::{
        CorsConfig, Policy, arrived_over_https, cors_layer, extendable_directive, websocket_origin,
    };
    use crate::config::AppConfig;

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
            csp: HeaderValue::from_static("default-src 'none'"),
        }
    }

    /// The policy a deployment with these variables set would send.
    fn rendered_policy(variables: &[(&str, &str)]) -> String {
        let owned: Vec<(String, String)> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let config = AppConfig::from_lookup(move |name| {
            owned
                .iter()
                .find(|(variable, _value)| variable == name)
                .map(|(_variable, value)| value.clone())
        })
        .expect("test configuration must parse");

        super::content_security_policy(&config)
            .to_str()
            .expect("the policy renders as ASCII")
            .to_owned()
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
    fn the_policy_names_every_directive_the_frontend_needs() {
        let policy = rendered_policy(&[("APP_URL", "https://app.example.com")]);

        // The base case is refusal, so a resource type nobody named is denied.
        assert!(policy.starts_with("default-src 'none'; "), "got: {policy}");
        // The build emits no inline script and compiles nothing at runtime.
        assert!(policy.contains("script-src 'self';"), "got: {policy}");
        assert!(!policy.contains("'unsafe-eval'"), "got: {policy}");
        // Runtime-injected stylesheets, some of which carry no nonce at all.
        assert!(
            policy.contains("style-src 'self' 'unsafe-inline';"),
            "got: {policy}",
        );
        // The avatar picker previews a chosen file through a blob: URL.
        assert!(
            policy.contains("img-src 'self' data: blob:;"),
            "got: {policy}"
        );
        // The realtime channel, named rather than left to `'self'` matching.
        assert!(
            policy.contains("connect-src 'self' wss://app.example.com;"),
            "got: {policy}",
        );
        assert!(policy.contains("base-uri 'self';"), "got: {policy}");
        assert!(policy.contains("form-action 'self';"), "got: {policy}");
        assert!(policy.ends_with("frame-ancestors 'none'"), "got: {policy}");
        // Directives with nothing to allow stay out: `default-src 'none'`
        // already refuses what they would have named.
        assert!(!policy.contains("frame-src"), "got: {policy}");
        assert!(!policy.contains("media-src"), "got: {policy}");
    }

    #[test]
    fn configured_sources_join_the_directives_they_name() {
        let policy = rendered_policy(&[
            ("APP_URL", "http://localhost:3000"),
            (
                "CSP_ALLOWED_SOURCES",
                "script-src https://plausible.io; connect-src https://plausible.io; \
                 frame-src https://js.stripe.com",
            ),
        ]);

        assert!(
            policy.contains("script-src 'self' https://plausible.io;"),
            "got: {policy}",
        );
        // Beside the framework's own sources, never instead of them.
        assert!(
            policy.contains("connect-src 'self' ws://localhost:3000 https://plausible.io;"),
            "got: {policy}",
        );
        // A directive with no framework sources renders exactly what was asked.
        assert!(
            policy.contains("frame-src https://js.stripe.com;"),
            "got: {policy}",
        );
    }

    #[test]
    fn the_websocket_origin_follows_the_public_url() {
        assert_eq!(
            websocket_origin("https://app.example.com").as_deref(),
            Some("wss://app.example.com"),
        );
        assert_eq!(
            websocket_origin("http://localhost:5173").as_deref(),
            Some("ws://localhost:5173"),
        );
        // A default port is not part of an origin, and CSP compares origins.
        assert_eq!(
            websocket_origin("https://app.example.com:443").as_deref(),
            Some("wss://app.example.com"),
        );
        assert_eq!(websocket_origin("not a url"), None);
    }

    /// The guard against a directive configuration accepts and rendering drops.
    #[test]
    fn every_extendable_directive_is_one_the_policy_renders() {
        for name in [
            "script-src",
            "style-src",
            "img-src",
            "font-src",
            "connect-src",
            "frame-src",
            "media-src",
        ] {
            assert_eq!(extendable_directive(name), Some(name));
            assert!(
                super::POLICY
                    .iter()
                    .any(|(directive, _base)| *directive == name),
                "{name} can be configured but the policy would never render it",
            );
        }
    }

    #[test]
    fn only_the_fetch_directives_can_be_extended() {
        assert_eq!(extendable_directive("script-src"), Some("script-src"));
        assert_eq!(extendable_directive("CONNECT-SRC"), Some("connect-src"));

        // Widening one of these from an environment variable would undo the
        // policy rather than extend it.
        assert_eq!(extendable_directive("default-src"), None);
        assert_eq!(extendable_directive("base-uri"), None);
        assert_eq!(extendable_directive("form-action"), None);
        assert_eq!(extendable_directive("frame-ancestors"), None);
        assert_eq!(extendable_directive("nonsense"), None);
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
