//! The resource-server half: turning `Authorization: Bearer` into a person.
//!
//! [`Bearer`] is to a connected client what [`crate::auth::CurrentUser`] is to
//! a browser: an extractor yielding the same [`User`], plus the scopes the
//! person granted and the client acting for them. A handler that takes it is
//! reachable by the client and by nothing else; a session cookie never
//! satisfies it, because a cookie is the browser's credential and a bearer
//! token is the client's.
//!
//! A token is honored when it is known, unexpired, its grant is live, and it
//! was issued for this server's resource (RFC 8707). A token bound to any other
//! resource is refused exactly like an unknown one, which is the audience
//! check the MCP specification requires of a resource server.
//!
//! Refusals answer `401` with a `WWW-Authenticate` challenge naming this
//! server's Protected Resource Metadata (RFC 9728), which is how a client that
//! has never connected finds the authorization server. A token lacking a scope
//! is answered `403` with `error="insufficient_scope"` and the scope it needs,
//! the step-up challenge the MCP specification describes.

use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use crate::auth::{User, token};
use crate::oauth_server::Server;
use crate::schema::{oauth_access_tokens, oauth_clients, oauth_grants, users};

/// A request a connected client made as a person.
#[derive(Debug, Clone)]
pub struct Bearer {
    /// The person the client acts for, the same row a session resolves to.
    pub user: User,
    /// The scopes this token carries.
    pub scopes: Vec<String>,
    /// The client's display name, as the person saw it at consent.
    pub client_name: String,
    /// The client's `client_id`.
    pub client_id: String,
    /// The grant the token descends from.
    pub grant_id: Uuid,
    /// Where a challenge sends the client to rediscover this server.
    resource_metadata: String,
}

impl Bearer {
    /// Whether the token carries `scope`.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }

    /// Requires `scope`, answering the step-up challenge when it is missing.
    ///
    /// # Errors
    /// Returns a `403` [`Challenge`] with `error="insufficient_scope"` naming
    /// `scope`, which tells the client to ask the person for it.
    pub fn require_scope(&self, scope: &str) -> Result<(), Challenge> {
        if self.has_scope(scope) {
            return Ok(());
        }
        Err(Challenge {
            status: StatusCode::FORBIDDEN,
            resource_metadata: self.resource_metadata.clone(),
            error: Some("insufficient_scope"),
            scope: Some(scope.to_owned()),
        })
    }
}

/// A refused bearer request, rendered with its `WWW-Authenticate` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    status: StatusCode,
    resource_metadata: String,
    error: Option<&'static str>,
    scope: Option<String>,
}

impl Challenge {
    /// The status the challenge answers with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The `WWW-Authenticate` value, RFC 6750 section 3 plus RFC 9728's
    /// `resource_metadata`.
    #[must_use]
    pub fn header_value(&self) -> String {
        let mut parts = vec![format!(
            "Bearer resource_metadata=\"{}\"",
            self.resource_metadata
        )];
        if let Some(error) = self.error {
            parts.push(format!("error=\"{error}\""));
        }
        if let Some(scope) = self.scope.as_deref().filter(|scope| !scope.is_empty()) {
            parts.push(format!("scope=\"{scope}\""));
        }
        parts.join(", ")
    }

    /// A `401` for a request with no usable token.
    ///
    /// `presented` says whether a token arrived at all: RFC 6750 names the
    /// error only when one did, so a client that has never connected reads a
    /// plain challenge and one whose token lapsed reads `invalid_token`.
    pub(crate) fn unauthorized(server: &Server, presented: bool) -> Self {
        let scope = server.scopes().names().join(" ");
        Self {
            status: StatusCode::UNAUTHORIZED,
            resource_metadata: server.resource_metadata_url(),
            error: presented.then_some("invalid_token"),
            scope: Some(scope),
        }
    }
}

impl IntoResponse for Challenge {
    fn into_response(self) -> Response {
        let header = HeaderValue::from_str(&self.header_value()).unwrap_or_else(|_error| {
            // Scope names are visible ASCII by construction and the metadata
            // URL comes from APP_URL, which parsed; a value that still fails
            // is answered with the bare scheme rather than no challenge.
            HeaderValue::from_static("Bearer")
        });
        let message = match self.status {
            StatusCode::FORBIDDEN => "This connection does not have the permission this needs.",
            _ => "Connect and sign in to use this.",
        };
        (
            self.status,
            [(WWW_AUTHENTICATE, header)],
            axum::Json(serde_json::json!({ "message": message })),
        )
            .into_response()
    }
}

impl<S> FromRequestParts<S> for Bearer
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(server) = parts.extensions.get::<Server>().cloned() else {
            tracing::error!(
                "Bearer used on a router without the authorization server; \
                 add .layer(Extension(server.clone())) to the router",
            );
            return Err(crate::http::ApiError::internal().into_response());
        };

        let presented = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                value
                    .split_once(' ')
                    .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                    .map(|(_, token)| token.trim().to_owned())
            })
            .filter(|token| !token.is_empty());
        let Some(presented) = presented else {
            return Err(Challenge::unauthorized(&server, false).into_response());
        };

        match authenticate(&server, &presented).await {
            Ok(Some(bearer)) => Ok(bearer),
            Ok(None) => Err(Challenge::unauthorized(&server, true).into_response()),
            Err(error) => {
                tracing::error!(
                    error.message = %error,
                    "bearer token lookup failed: {{error.message}}",
                );
                Err(crate::http::ApiError::internal().into_response())
            }
        }
    }
}

/// Resolves a raw access token, when it is live and bound to this resource.
async fn authenticate(
    server: &Server,
    presented: &str,
) -> Result<Option<Bearer>, Box<dyn std::error::Error + Send + Sync>> {
    let mut connection = server.pool().get().await?;
    let found: Option<(Vec<String>, Uuid, String, String, User)> = oauth_access_tokens::table
        .inner_join(
            oauth_grants::table
                .inner_join(oauth_clients::table)
                .inner_join(users::table),
        )
        .filter(oauth_access_tokens::token_hash.eq(token::hash(presented)))
        .filter(oauth_access_tokens::expires_at.gt(Utc::now()))
        .filter(oauth_grants::revoked_at.is_null())
        .filter(oauth_grants::resource.eq(server.resource()))
        .select((
            oauth_access_tokens::scopes,
            oauth_grants::id,
            oauth_clients::name,
            oauth_clients::client_id,
            User::as_select(),
        ))
        .first(&mut connection)
        .await
        .optional()?;

    Ok(
        found.map(|(scopes, grant_id, client_name, client_id, user)| Bearer {
            user,
            scopes,
            client_name,
            client_id,
            grant_id,
            resource_metadata: server.resource_metadata_url(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::Challenge;

    #[test]
    fn a_first_contact_challenge_names_the_metadata_and_the_scopes() {
        let challenge = Challenge {
            status: StatusCode::UNAUTHORIZED,
            resource_metadata: "https://app.example.com/.well-known/oauth-protected-resource/mcp"
                .to_owned(),
            error: None,
            scope: Some("samples:read entries:write".to_owned()),
        };

        assert_eq!(
            challenge.header_value(),
            "Bearer resource_metadata=\"https://app.example.com/.well-known/\
             oauth-protected-resource/mcp\", scope=\"samples:read entries:write\"",
        );
    }

    #[test]
    fn an_insufficient_scope_challenge_names_the_error_and_the_scope() {
        let challenge = Challenge {
            status: StatusCode::FORBIDDEN,
            resource_metadata: "https://app.example.com/.well-known/oauth-protected-resource/mcp"
                .to_owned(),
            error: Some("insufficient_scope"),
            scope: Some("entries:write".to_owned()),
        };

        assert!(
            challenge
                .header_value()
                .ends_with(", error=\"insufficient_scope\", scope=\"entries:write\""),
        );
    }
}
