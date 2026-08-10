//! Version 1 of the public REST API.
//!
//! The versioning contract: once users build against `/api/v1`, its handlers
//! and serializers freeze. A breaking change means minting a `v2` module
//! while this one continues to serve the old shapes, which is why v1 has its
//! own serializers instead of reusing the web-facing ones. The full policy
//! lives in the repository's `docs/api.md`.
//!
//! Requests authenticate with a platform application bearer token
//! (`Authorization: Bearer <token>`); the [`ApiCaller`] extractor resolves it
//! to the owning team, which scopes everything the caller may touch.

mod serializers;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json};
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_scalar::{Scalar, Servable};

use crate::api::platform::{self, PlatformApplication};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::tenancy::Team;

#[doc(inline)]
pub use serializers::TeamV1;

/// Returns the v1 API routes for an application to mount at `/api/v1`.
///
/// Alongside the endpoints, serves the OpenAPI 3.1 document at
/// `/openapi.json` and human-readable docs at `/docs`.
pub fn router(pool: DbPool) -> Router {
    let document = openapi();
    Router::new()
        .route("/team", get(show_team))
        .route(
            "/openapi.json",
            get({
                let document = document.clone();
                async move || Json(document)
            }),
        )
        .merge(Scalar::with_url("/docs", document))
        // ApiCaller resolves its pool from request extensions.
        .layer(Extension(pool))
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Anubis API",
        version = "1",
        description = "The versioned public REST API. Authenticate with a \
                       platform application bearer token from the Developers \
                       section.",
    ),
    paths(show_team),
    components(schemas(serializers::TeamV1, serializers::TeamEnvelopeV1, serializers::ErrorV1)),
    modifiers(&BearerTokenSecurity),
)]
struct ApiDoc;

struct BearerTokenSecurity;

impl Modify for BearerTokenSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "bearer_token",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
    }
}

/// The OpenAPI 3.1 document for v1 of the framework API.
///
/// Applications with scaffolded endpoints merge their own paths into this
/// document; the scaffolder maintains that wiring.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::openapi;

    #[test]
    fn the_document_describes_the_team_endpoint_and_bearer_auth() {
        let rendered = serde_json::to_string(&openapi()).expect("document must serialize");

        assert!(rendered.contains("\"openapi\":\"3.1"), "got: {rendered}");
        assert!(rendered.contains("/api/v1/team"), "got: {rendered}");
        assert!(rendered.contains("bearer_token"), "got: {rendered}");
        assert!(rendered.contains("TeamEnvelopeV1"), "got: {rendered}");
    }
}

/// The authenticated API caller: a platform application acting as its team.
#[derive(Debug, Clone)]
pub struct ApiCaller {
    /// The application whose token authenticated the request.
    pub application: PlatformApplication,
    /// The team the application belongs to; scopes everything the caller sees.
    pub team: Team,
}

impl<S> FromRequestParts<S> for ApiCaller
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(pool) = parts.extensions.get::<DbPool>().cloned() else {
            tracing::error!(
                "ApiCaller used on a router without a DbPool extension; \
                 add .layer(Extension(pool.clone())) to the router",
            );
            return Err(ApiError::internal());
        };

        let raw_token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(invalid_token)?;

        let mut connection = pool.get().await.map_err(|error| {
            tracing::error!(
                error.message = %error,
                "bearer token lookup failed: {{error.message}}",
            );
            ApiError::internal()
        })?;

        let resolved = platform::resolve_bearer(&mut connection, raw_token)
            .await
            .map_err(|error| {
                tracing::error!(
                    error.message = %error,
                    "bearer token lookup failed: {{error.message}}",
                );
                ApiError::internal()
            })?;

        let (application, team) = resolved.ok_or_else(invalid_token)?;
        Ok(Self { application, team })
    }
}

fn invalid_token() -> ApiError {
    ApiError::unauthorized("Provide a valid bearer token.")
}

/// The team the caller's token belongs to.
#[utoipa::path(
    get,
    path = "/api/v1/team",
    tag = "teams",
    responses(
        (status = 200, description = "The caller's team", body = serializers::TeamEnvelopeV1),
        (status = 401, description = "Missing or invalid bearer token", body = serializers::ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn show_team(caller: ApiCaller) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(serializers::TeamEnvelopeV1 {
        team: TeamV1::from(&caller.team),
    }))
}
