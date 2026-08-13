//! Serving a built single-page application behind the API routers.
//!
//! Needs no database: it composes a router the way the starter's `main` does,
//! with stand-in routes at the prefixes the framework mounts, and drives it
//! against a temporary directory that looks like a Vite build output.

use std::fs;
use std::path::{Path, PathBuf};

use anubis::spa::Assets;
use axum::Router;
use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// What a client-side route must hand the browser.
const INDEX_HTML: &str = "<!doctype html><title>Anubis</title><div id=\"root\"></div>";

/// A content-hashed asset, the kind Vite writes into `assets/`.
const HASHED_ASSET: &str = "console.log('anubis')";

/// The hashed asset's filename, mirroring Vite's `name-[hash].js`.
const HASHED_ASSET_PATH: &str = "/assets/index-a1b2c3.js";

/// A temporary build output, removed when the test that made it ends.
struct BuildOutput {
    root: PathBuf,
}

impl BuildOutput {
    /// Writes the smallest directory that passes for a frontend build.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("anubis-spa-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("assets")).expect("the build output must be creatable");
        fs::write(root.join("index.html"), INDEX_HTML).expect("index.html must be writable");
        fs::write(root.join("assets").join("index-a1b2c3.js"), HASHED_ASSET)
            .expect("the asset must be writable");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for BuildOutput {
    fn drop(&mut self) {
        // A leftover temp directory is not worth failing a passing test over.
        let _ignored = fs::remove_dir_all(&self.root);
    }
}

/// Composes the router the way the starter's `main` does.
///
/// The stand-in routes stand where the framework's real routers mount, so the
/// precedence this proves is the precedence production gets.
fn app(build: &BuildOutput) -> Router {
    let assets = Assets::new(build.path())
        .expect("the temporary build output must open")
        .reserve("/account");

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/auth/login", post(|| async { StatusCode::NO_CONTENT }))
        .route("/api/v1/things", get(|| async { axum::Json(json!([])) }))
        .fallback_service(assets.into_service())
}

struct Answer {
    status: StatusCode,
    cache_control: String,
    content_type: String,
    body: String,
}

impl Answer {
    fn is_index(&self) -> bool {
        self.body == INDEX_HTML
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

async fn send(router: &Router, method: &str, path: &str) -> Answer {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");

    let status = response.status();
    let header = |name: &axum::http::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let cache_control = header(&CACHE_CONTROL);
    let content_type = header(&CONTENT_TYPE);

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();

    Answer {
        status,
        cache_control,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

#[tokio::test]
async fn the_document_root_serves_the_application_shell() {
    let build = BuildOutput::new();
    let app = app(&build);

    let answer = send(&app, "GET", "/").await;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.is_index(), "got: {}", answer.body);
    assert!(
        answer.content_type.contains("text/html"),
        "got: {}",
        answer.content_type
    );
    // index.html names the hashed files, so it must never be cached blindly.
    assert_eq!(answer.cache_control, "no-cache");
}

#[tokio::test]
async fn client_side_routes_serve_the_application_shell() {
    let build = BuildOutput::new();
    let app = app(&build);

    for path in ["/settings/profile", "/creative-concepts/17", "/sign-in"] {
        let answer = send(&app, "GET", path).await;

        assert_eq!(answer.status, StatusCode::OK, "for {path}");
        assert!(answer.is_index(), "for {path}, got: {}", answer.body);
        assert_eq!(answer.cache_control, "no-cache", "for {path}");
    }
}

#[tokio::test]
async fn hashed_assets_are_served_with_their_type_and_cached_forever() {
    let build = BuildOutput::new();
    let app = app(&build);

    let answer = send(&app, "GET", HASHED_ASSET_PATH).await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body, HASHED_ASSET);
    assert!(
        answer.content_type.contains("javascript"),
        "got: {}",
        answer.content_type
    );
    assert_eq!(answer.cache_control, "public, max-age=31536000, immutable");
}

#[tokio::test]
async fn a_missing_hashed_asset_is_not_answered_with_html() {
    let build = BuildOutput::new();
    let app = app(&build);

    // Answering index.html here would cache HTML under a JavaScript URL for a
    // year, and no redeploy could reach the browsers that saw it.
    let answer = send(&app, "GET", "/assets/index-deadbeef.js").await;

    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(!answer.is_index(), "got: {}", answer.body);
}

#[tokio::test]
async fn mounted_routes_win_over_the_shell() {
    let build = BuildOutput::new();
    let app = app(&build);

    let healthz = send(&app, "GET", "/healthz").await;
    assert_eq!(healthz.status, StatusCode::OK);
    assert_eq!(healthz.body, "ok");

    let api = send(&app, "GET", "/api/v1/things").await;
    assert_eq!(api.status, StatusCode::OK);
    assert_eq!(api.json(), json!([]));

    // A real route answering the wrong method still answers, never the shell.
    let login = send(&app, "GET", "/auth/login").await;
    assert_eq!(login.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(!login.is_index(), "got: {}", login.body);

    let logged_in = send(&app, "POST", "/auth/login").await;
    assert_eq!(logged_in.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unmatched_api_paths_answer_json_rather_than_the_shell() {
    let build = BuildOutput::new();
    let app = app(&build);

    for path in [
        "/api/v1/nonexistent",
        "/auth/nonexistent",
        "/tenancy/nonexistent",
        "/developers/nonexistent",
        "/users/nonexistent",
        "/account/nonexistent",
    ] {
        let answer = send(&app, "GET", path).await;

        assert_eq!(answer.status, StatusCode::NOT_FOUND, "for {path}");
        assert!(!answer.is_index(), "for {path}, got: {}", answer.body);
        assert!(
            answer.content_type.contains("application/json"),
            "for {path}, got: {}",
            answer.content_type
        );
        assert_eq!(answer.json()["message"], json!("Not found."), "for {path}");
    }
}

#[tokio::test]
async fn a_client_route_that_only_resembles_a_reserved_prefix_still_serves_the_shell() {
    let build = BuildOutput::new();
    let app = app(&build);

    for path in ["/authors", "/apiary", "/accounts-payable"] {
        let answer = send(&app, "GET", path).await;

        assert_eq!(answer.status, StatusCode::OK, "for {path}");
        assert!(answer.is_index(), "for {path}, got: {}", answer.body);
    }
}

#[tokio::test]
async fn writes_to_unmatched_paths_are_not_found() {
    let build = BuildOutput::new();
    let app = app(&build);

    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let answer = send(&app, method, "/random").await;

        assert_eq!(answer.status, StatusCode::NOT_FOUND, "for {method}");
        assert!(!answer.is_index(), "for {method}, got: {}", answer.body);
    }
}

#[tokio::test]
async fn a_directory_that_is_not_a_build_output_is_refused_at_startup() {
    let empty = std::env::temp_dir().join(format!("anubis-spa-empty-{}", Uuid::new_v4()));
    fs::create_dir_all(&empty).expect("the directory must be creatable");

    let error = Assets::new(&empty).expect_err("an empty directory is not a build");

    assert!(error.to_string().contains("index.html"), "got: {error}");
    let _ignored = fs::remove_dir_all(&empty);
}
