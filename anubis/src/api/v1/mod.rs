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

use crate::api::platform::{self, PlatformApplication};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::tenancy::Team;

#[doc(inline)]
pub use serializers::TeamV1;

/// Returns the v1 API routes for an application to mount at `/api/v1`.
pub fn router(pool: DbPool) -> Router {
    Router::new()
        .route("/team", get(show_team))
        // ApiCaller resolves its pool from request extensions.
        .layer(Extension(pool))
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

/// `GET /api/v1/team`: the team the caller's token belongs to.
async fn show_team(caller: ApiCaller) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(serializers::TeamEnvelopeV1 {
        team: TeamV1::from(&caller.team),
    }))
}
