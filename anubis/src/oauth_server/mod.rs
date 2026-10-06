//! This deployment as an OAuth 2.1 authorization server for third-party clients.
//!
//! [`crate::auth::oauth`] is the other direction: this application signing a
//! person in with Google. This module is how a person lets a program act for
//! them here. Its first customer is the Model Context Protocol: Claude Code,
//! Claude Desktop, and Codex discover this server from the MCP endpoint's
//! `401`, register or present a Client ID Metadata Document, send the person
//! through a consent screen, and come back with tokens bound to the endpoint.
//! [`crate::mcp`] is that endpoint.
//!
//! # Routes
//!
//! [`router`] mounts at the application's root, because the well-known paths
//! are fixed by RFC 8414 and RFC 9728:
//!
//! | Route | Effect |
//! |---|---|
//! | `GET /.well-known/oauth-authorization-server` | Authorization Server Metadata |
//! | `GET /.well-known/oauth-protected-resource` | Protected Resource Metadata |
//! | `GET /.well-known/oauth-protected-resource/mcp` | The same, at the path MCP clients try first |
//! | `POST /oauth/register` | Dynamic Client Registration, bounded; see [`client`] |
//! | `GET /oauth/authorize` | Start the authorization code flow |
//! | `GET /oauth/requests/{id}` | What the consent screen asks (session) |
//! | `POST /oauth/requests/{id}` | The person's decision (session) |
//! | `POST /oauth/token` | Exchange a code, or rotate a refresh token |
//! | `POST /oauth/revoke` | RFC 7009 revocation |
//! | `GET /oauth/connections` | The account's connected clients (session) |
//! | `DELETE /oauth/connections/{id}` | Revoke one (session) |
//!
//! # What is fixed, and why
//!
//! - **PKCE `S256` only, authorization code only, public clients only.** OAuth
//!   2.1 and the MCP specification together leave nothing else for these
//!   clients, and every option not offered is one nobody can misuse.
//! - **One resource**, `<APP_URL>/mcp`. The Protected Resource Metadata path,
//!   the challenge's URL, the `resource` every token is bound to, and the
//!   development proxy all name it, so it is one constant rather than a
//!   setting four places have to agree on.
//! - **Opaque tokens, hashed at rest.** An access token is looked up on every
//!   request, which is what makes revocation immediate; nothing a client holds
//!   is stored in a form it could present.
//! - **Scopes are the application's.** [`Scopes`] is declared in Rust where the
//!   application composes its routers, and the framework hardcodes none.
//!
//! The design and the security posture live in `docs/oauth-server.md`.

mod authorize;
mod bearer;
mod client;
mod connections;
mod document;
mod error;
mod grant;
mod metadata;
mod pkce;
mod redirect;
mod scope;
mod token;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::{Extension, Router};
use url::Url;

use crate::config::{AppConfig, Environment};
use crate::db::DbPool;
use crate::rate_limit::{Budget, RateLimiter};

#[doc(inline)]
pub use authorize::CONSENT_PATH;
#[doc(inline)]
pub use bearer::{Bearer, Challenge};
#[doc(inline)]
pub use scope::{Scope, Scopes};

/// The path of the one resource this server issues tokens for.
///
/// The MCP endpoint lives here; see the module docs for why it is fixed.
pub const RESOURCE_PATH: &str = "/mcp";

/// The authorization server's configuration and shared state.
///
/// Cheap to clone: one `Arc` around the pool, the issuer, the resource, the
/// declared scopes, and the metadata-document fetcher. The MCP router takes
/// the same value, which is how the endpoint and the server agree on the
/// resource every token is bound to.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    pool: DbPool,
    issuer: String,
    resource: String,
    scopes: Scopes,
    documents: document::Fetcher,
}

impl std::fmt::Debug for Server {
    /// Names what identifies the server; the pool renders as noise.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("issuer", &self.inner.issuer)
            .field("resource", &self.inner.resource)
            .field("scopes", &self.inner.scopes)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Builds the server for this deployment.
    ///
    /// The issuer is `APP_URL`, the origin people and clients reach the
    /// application at, and the resource is that origin plus [`RESOURCE_PATH`].
    /// Outside production, Client ID Metadata Documents may be fetched from
    /// loopback hosts over `http`, which is what a test or a local client
    /// serving its own document needs; production fetches public `https` only.
    #[must_use]
    pub fn new(pool: DbPool, config: &AppConfig, scopes: Scopes) -> Self {
        let issuer = config.app_url.trim_end_matches('/').to_owned();
        let resource = format!("{issuer}{RESOURCE_PATH}");
        let allow_loopback = config.environment != Environment::Production;

        Self {
            inner: Arc::new(Inner {
                pool,
                issuer,
                resource,
                scopes,
                documents: document::Fetcher::new(allow_loopback),
            }),
        }
    }

    /// The issuer identifier, `APP_URL` without a trailing slash.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// The resource every token is bound to, `<issuer>/mcp`.
    #[must_use]
    pub fn resource(&self) -> &str {
        &self.inner.resource
    }

    /// The scopes the application declared.
    #[must_use]
    pub fn scopes(&self) -> &Scopes {
        &self.inner.scopes
    }

    /// The URL of this resource's Protected Resource Metadata.
    #[must_use]
    pub fn resource_metadata_url(&self) -> String {
        format!(
            "{}/.well-known/oauth-protected-resource{RESOURCE_PATH}",
            self.inner.issuer
        )
    }

    /// Whether a client's `resource` parameter names this server's resource.
    ///
    /// Compared after parsing, so the uppercase scheme or host the MCP
    /// specification says to accept still matches, and a trailing slash is
    /// forgiven; anything else is another resource.
    pub(crate) fn is_resource(&self, presented: &str) -> bool {
        Url::parse(presented).is_ok_and(|url| {
            url.fragment().is_none() && url.as_str().trim_end_matches('/') == self.inner.resource
        })
    }

    pub(crate) fn pool(&self) -> &DbPool {
        &self.inner.pool
    }

    pub(crate) fn documents(&self) -> &document::Fetcher {
        &self.inner.documents
    }
}

/// Returns the authorization server's routes, to merge at the root.
///
/// The limiter is the application's one [`RateLimiter`], because registration
/// is charged per client address like every other unauthenticated write.
/// Session routes resolve [`crate::auth::CurrentUser`] through the pool this
/// router layers on itself.
pub fn router(server: &Server, rate_limit: &RateLimiter) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(metadata::authorization_server),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(metadata::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(metadata::protected_resource),
        )
        .route(
            "/oauth/register",
            post(client::register).layer(rate_limit.layer(Budget::ClientRegistration)),
        )
        .route("/oauth/authorize", get(authorize::authorize))
        .route(
            "/oauth/requests/{request_id}",
            get(authorize::show).post(authorize::decide),
        )
        .route("/oauth/token", post(token::exchange))
        .route("/oauth/revoke", post(token::revoke))
        .route("/oauth/connections", get(connections::list))
        .route(
            "/oauth/connections/{grant_id}",
            axum::routing::delete(connections::revoke),
        )
        .with_state(server.clone())
        .layer(Extension(server.pool().clone()))
}

#[cfg(test)]
mod tests {
    use diesel_async::AsyncPgConnection;
    use diesel_async::pooled_connection::AsyncDieselConnectionManager;
    use diesel_async::pooled_connection::deadpool::Pool;

    use super::{Scopes, Server};

    fn server(app_url: &str) -> Server {
        let config = crate::config::AppConfig::from_lookup(|name| match name {
            "ANUBIS_ENV" => Some("test".to_owned()),
            "APP_URL" => Some(app_url.to_owned()),
            _ => None,
        })
        .expect("the test config parses");
        // Building a pool connects to nothing, and these tests never ask it
        // for a connection.
        let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(
            "postgres://unused@127.0.0.1:9/unused",
        );
        let pool = Pool::builder(manager).build().expect("the pool builds");
        Server::new(pool, &config, Scopes::new())
    }

    #[test]
    fn the_resource_is_the_mcp_endpoint_under_app_url() {
        let server = server("https://app.example.com/");

        assert_eq!(server.issuer(), "https://app.example.com");
        assert_eq!(server.resource(), "https://app.example.com/mcp");
        assert_eq!(
            server.resource_metadata_url(),
            "https://app.example.com/.well-known/oauth-protected-resource/mcp",
        );
    }

    #[test]
    fn a_resource_matches_in_any_case_and_with_a_trailing_slash() {
        let server = server("https://app.example.com");

        assert!(server.is_resource("https://app.example.com/mcp"));
        assert!(server.is_resource("HTTPS://APP.EXAMPLE.COM/mcp"));
        assert!(server.is_resource("https://app.example.com/mcp/"));
        assert!(!server.is_resource("https://app.example.com/MCP"));
        assert!(!server.is_resource("https://evil.example/mcp"));
        assert!(!server.is_resource("https://app.example.com/mcp#x"));
        assert!(!server.is_resource("https://app.example.com"));
    }
}
