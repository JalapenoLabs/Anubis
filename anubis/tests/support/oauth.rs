//! A connected client, played by the test: the steps Claude Code takes.
//!
//! The authorization server fetches a client's metadata document from the URL
//! that is its `client_id`, so [`MetadataDocument`] serves one on a loopback
//! port, which the server allows outside production. The rest are the
//! requests a client and the person's browser make, in the order they make
//! them, so a narrative reads as the ceremony it is.

use std::net::SocketAddr;

use axum::body::Body;
use axum::http::header::{ACCEPT, CONTENT_TYPE, COOKIE, HOST, LOCATION};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;

use super::harness::send;

/// The RFC 7636 appendix B verifier, and the challenge it answers.
pub const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
/// `BASE64URL(SHA256(VERIFIER))`.
pub const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// The registered redirect URI, port-less the way Claude Code registers it.
pub const REGISTERED_REDIRECT: &str = "http://localhost/callback";
/// What the client presents at sign-in time, with the port it bound.
pub const REDIRECT: &str = "http://localhost:53682/callback";

/// The origin the harness's `APP_URL` names, which the MCP endpoint checks
/// `Host` against.
pub const APP_HOST: &str = "127.0.0.1:3000";
/// The resource every token is bound to.
pub const RESOURCE: &str = "http://127.0.0.1:3000/mcp";

/// A Client ID Metadata Document served on a loopback port.
#[derive(Debug, Clone)]
pub struct MetadataDocument {
    /// The document's URL, which is the client's `client_id`.
    pub client_id: String,
}

impl MetadataDocument {
    /// Serves a document naming `name` and the port-less loopback redirects.
    ///
    /// # Panics
    /// Panics when no loopback port is available.
    pub fn serve(name: &str) -> Self {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port must be available");
        let address: SocketAddr = listener.local_addr().expect("the listener is bound");
        drop(listener);

        let client_id = format!("http://127.0.0.1:{}/client.json", address.port());
        let document = json!({
            "client_id": client_id,
            "client_name": name,
            "client_uri": "https://example.com",
            "redirect_uris": [REGISTERED_REDIRECT, "http://127.0.0.1/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        let router = Router::new().route(
            "/client.json",
            get(move || {
                let document = document.clone();
                async move { Json(document) }
            }),
        );

        let listener =
            std::net::TcpListener::bind(address).expect("the reserved port must still be free");
        listener
            .set_nonblocking(true)
            .expect("the listener must go non-blocking");
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("the listener adopts the socket");
        tokio::spawn(async move {
            let _served = axum::serve(listener, router).await;
        });

        Self { client_id }
    }
}

/// The query an authorization request carries, before any test edits it.
#[must_use]
pub fn authorize_params(client_id: &str, scope: &str) -> Vec<(&'static str, String)> {
    vec![
        ("response_type", "code".to_owned()),
        ("client_id", client_id.to_owned()),
        ("redirect_uri", REDIRECT.to_owned()),
        ("scope", scope.to_owned()),
        ("state", "client-state".to_owned()),
        ("code_challenge", CHALLENGE.to_owned()),
        ("code_challenge_method", "S256".to_owned()),
        ("resource", RESOURCE.to_owned()),
    ]
}

/// Sends the browser to `/oauth/authorize`, answering where it was sent.
///
/// # Panics
/// Panics when the endpoint answers anything but a redirect.
pub async fn authorize(router: &Router, params: &[(&str, String)]) -> Url {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter().map(|(name, value)| (*name, value.as_str())))
        .finish();
    let response = router
        .clone()
        .oneshot(
            Request::get(format!("/oauth/authorize?{query}"))
                .body(Body::empty())
                .expect("the request builds"),
        )
        .await
        .expect("the router answers");

    assert!(
        response.status().is_redirection(),
        "authorize must redirect, got {}",
        response.status(),
    );
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("a redirect carries a location")
        .to_owned();
    Url::parse(&location)
        .or_else(|_relative| {
            Url::parse("http://127.0.0.1:3000").and_then(|base| base.join(&location))
        })
        .expect("the location is a URL")
}

/// One query parameter of `url`.
#[must_use]
pub fn param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// Approves or denies a consent request as the signed-in person.
///
/// # Panics
/// Panics when the decision is refused.
pub async fn decide(router: &Router, cookie: &str, request_id: &str, approve: bool) -> Url {
    let (status, _headers, body) = send(
        router,
        "POST",
        &format!("/oauth/requests/{request_id}"),
        Some(&json!({ "approve": approve })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    Url::parse(
        body["redirect_to"]
            .as_str()
            .expect("a decision says where to go"),
    )
    .expect("the destination is a URL")
}

/// Posts a form, as a client calls the token and revocation endpoints.
///
/// # Panics
/// Panics when the request cannot be built or answered.
pub async fn post_form(router: &Router, path: &str, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let response = router
        .clone()
        .oneshot(
            Request::post(path)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .expect("the request builds"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body collects")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Exchanges a code for tokens.
pub async fn exchange(router: &Router, client_id: &str, code: &str) -> (StatusCode, Value) {
    post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
            ("resource", RESOURCE),
        ],
    )
    .await
}

/// Rotates a refresh token.
pub async fn refresh(router: &Router, client_id: &str, refresh_token: &str) -> (StatusCode, Value) {
    post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await
}

/// The tokens a finished ceremony leaves the client holding.
#[derive(Debug, Clone)]
pub struct Connection {
    pub access_token: String,
    pub refresh_token: String,
}

/// Runs the whole ceremony: authorize, consent, exchange.
///
/// # Panics
/// Panics when any step is refused.
pub async fn connect(router: &Router, cookie: &str, client_id: &str, scope: &str) -> Connection {
    let consent = authorize(router, &authorize_params(client_id, scope)).await;
    let request_id = param(&consent, "request").expect("authorize opens the consent screen");
    let callback = decide(router, cookie, &request_id, true).await;
    let code = param(&callback, "code").expect("an approval delivers a code");

    let (status, tokens) = exchange(router, client_id, &code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    Connection {
        access_token: tokens["access_token"]
            .as_str()
            .expect("an access token")
            .to_owned(),
        refresh_token: tokens["refresh_token"]
            .as_str()
            .expect("a refresh token")
            .to_owned(),
    }
}

/// Sends one JSON-RPC message to the MCP endpoint, as a client would.
///
/// `access_token` is the bearer, or nothing for a client that has not
/// connected yet. `extra` adds headers such as the 2026 revision's
/// `MCP-Protocol-Version` and `Mcp-Method`.
///
/// # Panics
/// Panics when the request cannot be built or answered.
pub async fn mcp(
    router: &Router,
    access_token: Option<&str>,
    message: &Value,
    extra: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::post("/mcp")
        .header(HOST, APP_HOST)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream");
    if let Some(token) = access_token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }

    let response = router
        .clone()
        .oneshot(
            builder
                .body(Body::from(
                    serde_json::to_vec(message).expect("the message serializes"),
                ))
                .expect("the request builds"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body collects")
        .to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Sends a request carrying a session cookie and no bearer to `/mcp`.
pub async fn mcp_with_cookie(router: &Router, cookie: &str, message: &Value) -> StatusCode {
    let response = router
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(HOST, APP_HOST)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json, text/event-stream")
                .header(COOKIE, format!("anubis_session={cookie}"))
                .body(Body::from(
                    serde_json::to_vec(message).expect("the message serializes"),
                ))
                .expect("the request builds"),
        )
        .await
        .expect("the router answers");
    response.status()
}
