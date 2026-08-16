//! Serving the built single-page application from the API binary.
//!
//! A production deployment is one process: the same server that answers
//! `/api/v1` hands the browser the compiled React app. [`Assets`] turns a
//! directory of built frontend files into the router fallback that does it.
//! Development does not use this at all; there the Vite dev server owns the
//! browser and proxies the API.
//!
//! # Routing
//!
//! The service is mounted as the router's *fallback*, so it runs only after
//! every mounted route has declined the request. That is what makes API
//! precedence automatic: `/api/v1/projects` is claimed by the API router and
//! never reaches here. What does reach here is one of four things:
//!
//! - a file that exists under the assets directory, served as itself,
//! - a client-side route such as `/settings/profile`, served `index.html` so a
//!   cold load lands on the right page,
//! - a request under a reserved prefix, answered with a JSON `404`,
//! - a request that is not a `GET` or `HEAD`, answered with a JSON `404`.
//!
//! # Reserved prefixes
//!
//! An unmatched `/api/v1/typo` must not answer `200 OK` with `index.html`. The
//! caller asked for JSON, and HTML turns a routing mistake into a parse error
//! somewhere far away, or worse, into a silent success. So the fallback
//! refuses to serve the SPA under the prefixes the framework mounts its
//! routers on ([`RESERVED_PREFIXES`]), answering the same JSON `404` shape the
//! rest of the API uses.
//!
//! A prefix list is the mechanism because the alternative, giving every
//! mounted router its own fallback, spreads one decision across every mount
//! and silently regresses the moment a new one forgets. Applications reserve
//! their own JSON prefixes with [`Assets::reserve`]. Prefixes are matched on
//! whole path segments, so reserving `/auth` claims `/auth` and `/auth/login`
//! but leaves a client route named `/authors` alone.
//!
//! # Caching
//!
//! Vite writes content-hashed filenames into `assets/`, so a URL there names
//! exactly one build of exactly one file forever: those responses carry a
//! one-year `immutable` `Cache-Control`. Everything else, `index.html` above
//! all, carries `no-cache`, because `index.html` is what names the hashed
//! files. Browsers revalidate it on every load, so a deploy is live as soon as
//! it lands rather than whenever caches happen to expire.
//!
//! A hashed asset that is *missing* answers `404` instead of `index.html`.
//! Otherwise a half-copied deploy would cache HTML under a JavaScript URL for
//! a year, and no amount of redeploying would fix the browsers that saw it.
//!
//! # Examples
//!
//! ```no_run
//! use axum::Router;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let app: Router = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
//!
//! // Everything the API did not claim resolves to the SPA.
//! let assets = anubis::spa::Assets::new("frontend/dist")?.reserve("/account");
//! let app = app.fallback_service(assets.into_service());
//! # let _ = app;
//! # Ok(())
//! # }
//! ```

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, any};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::http::ApiError;

/// The file a bundler emits as the application shell.
const INDEX_FILE: &str = "index.html";

/// Where Vite writes content-hashed filenames, relative to the build output.
///
/// Changing it means changing `build.assetsDir` in `vite.config.ts` too;
/// nothing else in the stack decides what may be cached forever.
const HASHED_ASSETS_PREFIX: &str = "/assets/";

/// Caching for content-hashed filenames.
///
/// A year is the longest `max-age` HTTP caches honor, and `immutable` tells
/// them not to revalidate even on a reload. Safe only because the filename
/// changes whenever the bytes do.
const IMMUTABLE: HeaderValue = HeaderValue::from_static("public, max-age=31536000, immutable");

/// Caching for everything else, `index.html` above all.
///
/// `no-cache` permits storing the response but requires revalidation before
/// reuse, so a returning browser picks up a new deploy on its next load while
/// still paying nothing for an unchanged one.
const REVALIDATE: HeaderValue = HeaderValue::from_static("no-cache");

/// The path prefixes the framework mounts routers on.
///
/// Requests under these never resolve to `index.html`; see the module docs.
/// Drift cannot happen silently: a unit test walks
/// [`crate::manifest::framework_routes`] and fails the build if a mounted
/// route is not covered here. The probes `/healthz` and `/readyz` are
/// deliberately absent, being exact routes with nothing nested under them:
/// they always match their own router and so can never reach the fallback.
pub const RESERVED_PREFIXES: &[&str] = &[
    "/api",
    "/auth",
    "/billing",
    "/developers",
    "/realtime",
    "/tenancy",
    "/users",
    // The framework's own Stripe billing receiver lives here, and an
    // application's scaffolded receivers mount beside it. A provider posting to
    // a mistyped path deserves a JSON `404` it can act on rather than an HTML
    // page and a `200`.
    "/webhooks",
];

/// The built frontend, served as a router fallback.
///
/// Construct it once at startup with [`Assets::new`], which fails when the
/// directory is not a build output, then hand [`Assets::into_service`] to
/// [`axum::Router::fallback_service`].
#[derive(Debug, Clone)]
pub struct Assets {
    inner: Arc<Inner>,
}

#[derive(Debug, Clone)]
struct Inner {
    files: ServeDir,
    index: ServeFile,
    reserved: Vec<String>,
}

impl Assets {
    /// Opens a directory of built frontend files.
    ///
    /// Startup is the only good moment to discover that a deployment shipped
    /// without a frontend build, so the directory and its `index.html` are
    /// checked here rather than on the first request.
    ///
    /// # Errors
    /// Returns an [`Error`] when `root` is not a directory, or when it holds
    /// no `index.html`.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, Error> {
        let root = root.as_ref();

        if !root.is_dir() {
            return Err(Error::new(
                root,
                "is not a directory; build the frontend first",
            ));
        }

        let index = root.join(INDEX_FILE);
        if !index.is_file() {
            return Err(Error::new(
                root,
                "holds no index.html, so it is not a frontend build output",
            ));
        }

        Ok(Self {
            inner: Arc::new(Inner {
                files: ServeDir::new(root),
                index: ServeFile::new(index),
                reserved: RESERVED_PREFIXES
                    .iter()
                    .map(|&prefix| prefix.into())
                    .collect(),
            }),
        })
    }

    /// Reserves one more path prefix for JSON, on top of [`RESERVED_PREFIXES`].
    ///
    /// Applications call this for the prefixes they mount their own routers
    /// on, so an unmatched path there answers a JSON `404` instead of the SPA.
    /// A leading slash is optional and a trailing one is ignored.
    ///
    /// # Panics
    /// Panics when the prefix is empty, which would reserve the whole site and
    /// leave the SPA unreachable.
    #[must_use]
    pub fn reserve(mut self, prefix: impl AsRef<str>) -> Self {
        Arc::make_mut(&mut self.inner)
            .reserved
            .push(normalize_prefix(prefix.as_ref()));
        self
    }

    /// Turns the assets into the service to hand `fallback_service`.
    pub fn into_service(self) -> MethodRouter {
        any(serve).with_state(self)
    }

    /// Answers one request that no mounted route claimed.
    async fn respond(&self, request: Request) -> Response {
        // Static assets answer reads. Any other method that got this far is a
        // routing mistake, and index.html would only disguise it.
        let method = request.method().clone();
        if method != Method::GET && method != Method::HEAD {
            return ApiError::not_found().into_response();
        }

        let path = request.uri().path().to_owned();
        if is_reserved(&path, &self.inner.reserved) {
            return ApiError::not_found().into_response();
        }

        let served = match self.inner.files.clone().oneshot(request).await {
            Ok(response) => response,
            Err(infallible) => match infallible {},
        };

        if served.status() == StatusCode::NOT_FOUND {
            // See the module docs: a missing hashed asset is a broken deploy,
            // not a client route, and must never be cached as HTML.
            if path.starts_with(HASHED_ASSETS_PREFIX) {
                return ApiError::not_found().into_response();
            }
            return self.index(method).await;
        }

        cached(served.map(Body::new), cache_control(&path))
    }

    /// Serves the application shell, the answer to every client-side route.
    async fn index(&self, method: Method) -> Response {
        let request = Request::builder()
            .method(method)
            .uri("/")
            .body(Body::empty())
            .expect("a read of the index is always a valid request");

        // ServeFile serves its one file whatever the URI says.
        let served = match self.inner.index.clone().oneshot(request).await {
            Ok(response) => response,
            Err(infallible) => match infallible {},
        };

        cached(served.map(Body::new), REVALIDATE)
    }
}

/// Renders a caller's prefix as a rooted path with no trailing slash.
///
/// # Panics
/// Panics on a prefix that names no segment; see [`Assets::reserve`].
fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix('/').unwrap_or(trimmed);
    assert!(
        !trimmed.is_empty(),
        "a reserved prefix must name a path segment, such as /account",
    );
    format!("/{trimmed}")
}

/// Returns `true` when a path belongs to a router rather than to the SPA.
///
/// Matching is by whole path segment, so `/auth` claims `/auth/login` but not
/// a client route named `/authors`.
fn is_reserved(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        path.strip_prefix(prefix.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The axum handler behind [`Assets::into_service`].
async fn serve(State(assets): State<Assets>, request: Request) -> Response {
    assets.respond(request).await
}

/// Picks the caching policy a path's contents deserve.
fn cache_control(path: &str) -> HeaderValue {
    if path.starts_with(HASHED_ASSETS_PREFIX) {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

/// Stamps the caching policy onto a response, replacing any the file service set.
fn cached(mut response: Response, policy: HeaderValue) -> Response {
    response.headers_mut().insert(header::CACHE_CONTROL, policy);
    response
}

/// A directory does not hold a usable frontend build.
#[derive(Debug)]
pub struct Error {
    root: PathBuf,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    fn new(root: &Path, problem: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            message: format!("the frontend assets directory {} {problem}", root.display()),
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns the directory that was asked for.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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
    use std::path::Path;

    use super::{Assets, RESERVED_PREFIXES, cache_control, is_reserved, normalize_prefix};

    /// The framework routes that need no reserved prefix, being exact paths
    /// with nothing nested under them.
    const PROBES: &[&str] = &["/healthz", "/readyz"];

    fn reserved() -> Vec<String> {
        RESERVED_PREFIXES
            .iter()
            .map(|&prefix| prefix.to_owned())
            .collect()
    }

    #[test]
    fn a_directory_without_an_index_is_not_a_build_output() {
        let crate_root = env!("CARGO_MANIFEST_DIR");

        let error = Assets::new(crate_root).expect_err("the crate root is not a build");

        assert!(error.to_string().contains("index.html"), "got: {error}");
        assert_eq!(error.root(), Path::new(crate_root));
    }

    #[test]
    fn a_missing_directory_is_rejected() {
        let error = Assets::new("this/does/not/exist").expect_err("nothing is there");

        assert!(
            error.to_string().contains("not a directory"),
            "got: {error}"
        );
    }

    #[test]
    fn hashed_assets_are_cached_forever_and_nothing_else_is() {
        assert_eq!(
            cache_control("/assets/index-a1b2c3.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("/"), "no-cache");
        assert_eq!(cache_control("/settings/profile"), "no-cache");
        // A path that merely starts with those letters is not the assets folder.
        assert_eq!(cache_control("/assetstore"), "no-cache");
    }

    #[test]
    fn reserved_prefixes_match_whole_segments() {
        let reserved = reserved();

        assert!(is_reserved("/api", &reserved));
        assert!(is_reserved("/api/v1/projects", &reserved));
        assert!(is_reserved("/auth/login", &reserved));
        assert!(!is_reserved("/authors", &reserved));
        assert!(!is_reserved("/apiary", &reserved));
        assert!(!is_reserved("/settings/profile", &reserved));
        assert!(!is_reserved("/", &reserved));
    }

    /// The guard against a new framework mount the SPA would shadow.
    #[test]
    fn every_framework_mount_is_reserved() {
        let reserved = reserved();

        for route in crate::manifest::framework_routes() {
            assert!(
                is_reserved(route.path, &reserved) || PROBES.contains(&route.path),
                "{} is mounted, so an unmatched sibling of it must answer JSON, \
                 not the SPA: add its prefix to RESERVED_PREFIXES",
                route.path,
            );
        }
    }

    /// The other half of the guard above: development must reach what
    /// production claims.
    ///
    /// One binary serves the SPA and the API in production, while development
    /// runs two servers and Vite proxies the API half. A prefix mounted here
    /// but missing from that proxy list works when deployed and answers the
    /// dev server's own `404` locally, which is the worst way to find out. The
    /// check is textual because the two sides are different languages, and the
    /// starter's config is what `anubis new` stamps into every application, so
    /// this covers them too.
    #[test]
    fn the_starters_dev_proxy_reaches_every_reserved_prefix() {
        let config = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../starter/frontend/vite.config.ts"
        ))
        .expect("the starter's vite config is readable");

        for path in RESERVED_PREFIXES.iter().chain(PROBES) {
            assert!(
                config.contains(&format!("'{path}':")),
                "the starter's vite dev server does not proxy {path}: \
                 add it to the proxy list in starter/frontend/vite.config.ts",
            );
        }
    }

    #[test]
    fn a_reserved_prefix_is_rooted_and_unslashed() {
        assert_eq!(normalize_prefix("account"), "/account");
        assert_eq!(normalize_prefix("/account"), "/account");
        assert_eq!(normalize_prefix(" /account/ "), "/account");
    }

    #[test]
    #[should_panic(expected = "must name a path segment")]
    fn reserving_the_whole_site_is_a_programming_error() {
        let _reserved = normalize_prefix("/");
    }
}
