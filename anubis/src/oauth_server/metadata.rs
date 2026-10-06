//! The two discovery documents a client reads before it does anything else.
//!
//! - **Protected Resource Metadata** (RFC 9728) says which authorization server
//!   protects the MCP endpoint and which scopes it understands. It is served
//!   at the path-suffixed well-known URI the MCP specification tries first
//!   (`/.well-known/oauth-protected-resource/mcp`) and at the root one it
//!   falls back to, with the same body at both.
//! - **Authorization Server Metadata** (RFC 8414) says where to authorize,
//!   exchange, register, and revoke, and what this server supports.
//!
//! Four of its fields decide how Claude and Codex register, and all four have
//! to be present or both fall back to Dynamic Client Registration:
//! `client_id_metadata_document_supported`, `"none"` among the token
//! endpoint's authentication methods, `S256` as the only challenge method (a
//! client refuses to proceed without it), and
//! `authorization_response_iss_parameter_supported`, which this server backs
//! by sending `iss` on every authorization response.

use axum::Json;
use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::response::IntoResponse;
use serde_json::json;

use crate::oauth_server::{Server, pkce};

/// How long a client may cache either document.
///
/// An hour: the documents change when the application deploys a new scope,
/// and a client reading a stale list only asks for less than it could.
const METADATA_CACHE: &str = "public, max-age=3600";

/// `GET /.well-known/oauth-protected-resource[/mcp]`.
pub(crate) async fn protected_resource(State(server): State<Server>) -> impl IntoResponse {
    (
        [(CACHE_CONTROL, METADATA_CACHE)],
        Json(json!({
            "resource": server.resource(),
            "authorization_servers": [server.issuer()],
            "scopes_supported": server.scopes().names(),
            "bearer_methods_supported": ["header"],
        })),
    )
}

/// `GET /.well-known/oauth-authorization-server`.
pub(crate) async fn authorization_server(State(server): State<Server>) -> impl IntoResponse {
    let issuer = server.issuer();
    (
        [(CACHE_CONTROL, METADATA_CACHE)],
        Json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/oauth/authorize"),
            "token_endpoint": format!("{issuer}/oauth/token"),
            "registration_endpoint": format!("{issuer}/oauth/register"),
            "revocation_endpoint": format!("{issuer}/oauth/revoke"),
            "scopes_supported": server.scopes().names(),
            "response_types_supported": ["code"],
            "response_modes_supported": ["query"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_methods_supported": ["none"],
            "revocation_endpoint_auth_methods_supported": ["none"],
            "code_challenge_methods_supported": [pkce::METHOD],
            "client_id_metadata_document_supported": true,
            "authorization_response_iss_parameter_supported": true,
        })),
    )
}
