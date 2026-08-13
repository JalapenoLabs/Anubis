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
//!
//! ## Application endpoints
//!
//! An application owns its own v1 surface. [`openapi`] is the framework's half
//! of the document (this module's endpoints, the bearer security scheme, and
//! the shared schemas); the application merges its scaffolded paths into it and
//! mounts the result with [`router_with`], which serves the merged document at
//! `/api/v1/openapi.json` and `/api/v1/docs`. The application's own `lib.rs`
//! carries that composition, so a version freezes with the application rather
//! than with the framework release it was generated against.

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
use crate::roles::{Action, RoleSet};
use crate::tenancy::Team;

#[doc(inline)]
pub use serializers::{ErrorV1, TeamV1};

/// Returns the framework's v1 routes for an application to mount at `/api/v1`.
///
/// Alongside the endpoints, serves the OpenAPI 3.1 document at
/// `/openapi.json` and human-readable docs at `/docs`. An application with
/// scaffolded endpoints calls [`router_with`] instead.
pub fn router(pool: DbPool) -> Router {
    router_with(pool, Router::new(), openapi())
}

/// Returns the v1 routes, the application's own endpoints included.
///
/// `application` holds the application's `/api/v1` handlers, mounted beside the
/// framework's; `document` is the merged OpenAPI document served at
/// `/openapi.json` and rendered at `/docs`. Both come from the application, so
/// one call composes the whole version:
///
/// ```ignore
/// .nest(
///     "/api/v1",
///     anubis::api::v1::router_with(pool.clone(), api_v1_router(&pool, &roles), openapi()),
/// )
/// ```
pub fn router_with(
    pool: DbPool,
    application: Router,
    document: utoipa::openapi::OpenApi,
) -> Router {
    Router::new()
        .route("/team", get(show_team))
        .merge(application)
        .route(
            "/openapi.json",
            get({
                let document = document.clone();
                async move || Json(document)
            }),
        )
        .merge(Scalar::with_url("/docs", document))
        // ApiCaller resolves its pool from request extensions, and so do the
        // application's handlers when they take one.
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
    components(schemas(
        serializers::TeamV1,
        serializers::TeamEnvelopeV1,
        serializers::ErrorV1,
        crate::http::Pagination,
    )),
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
/// This is the base an application merges its own paths and schemas into. The
/// application's document wins on `info`, because the version belongs to the
/// application; everything here (the endpoints above, the bearer security
/// scheme, and the shared schemas) is added to it:
///
/// ```ignore
/// let mut document = ApiDoc::openapi();       // the application's own info
/// document.merge(anubis::api::v1::openapi()); // the framework's half
/// document.merge(projects::openapi());        // one line per scaffolded model
/// ```
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

/// Renders the generated TypeScript client for the framework's own document.
///
/// # Panics
/// Panics if the OpenAPI document fails to serialize, which would be a bug in
/// the document itself.
#[must_use]
pub fn typescript_client() -> String {
    let document = serde_json::to_value(openapi()).expect("the OpenAPI document must serialize");
    typescript_client_from(&document)
}

/// Renders the generated TypeScript client for an exported document.
///
/// An application exports its merged document (the starter's binary answers
/// `openapi` with it) and `anubis client generate-ts --from <file>` renders
/// that, so the client covers the application's endpoints as well as the
/// framework's.
#[must_use]
pub fn typescript_client_from(document: &serde_json::Value) -> String {
    crate::api::typescript::render(document)
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
        // The shapes an application's scaffolded endpoints answer with.
        assert!(rendered.contains("ErrorV1"), "got: {rendered}");
        assert!(rendered.contains("Pagination"), "got: {rendered}");
    }

    /// The merge an application performs: its identity, plus this half.
    #[test]
    fn an_application_keeps_its_own_info_and_gains_the_framework_half() {
        #[derive(utoipa::OpenApi)]
        #[openapi(info(title = "Acme API", version = "1"))]
        struct AppDoc;

        let mut document = <AppDoc as utoipa::OpenApi>::openapi();
        document.merge(openapi());
        let rendered = serde_json::to_value(&document).expect("document must serialize");

        assert_eq!(rendered["info"]["title"], "Acme API");
        assert!(rendered["paths"]["/api/v1/team"].is_object(), "{rendered}");
        assert!(
            rendered["components"]["securitySchemes"]["bearer_token"].is_object(),
            "{rendered}",
        );
    }
}

/// The roles a platform application's token exercises inside its team.
///
/// A token is created by a team admin and belongs to the team rather than to
/// any member, so it acts with the team's full rights, exactly as Bullet
/// Train's team-scoped tokens do. Authorization still runs through the
/// compiled `roles.yml`, so a model no role may touch is refused on the API
/// too, and a future per-application scope narrows this list rather than
/// introducing a second permission system.
pub const PLATFORM_APPLICATION_ROLES: [&str; 1] = ["admin"];

/// The authenticated API caller: a platform application acting as its team.
#[derive(Debug, Clone)]
pub struct ApiCaller {
    /// The application whose token authenticated the request.
    pub application: PlatformApplication,
    /// The team the application belongs to; scopes everything the caller sees.
    pub team: Team,
}

impl ApiCaller {
    /// Returns `true` when the token's roles grant the action on the model.
    #[must_use]
    pub fn can(&self, roles: &RoleSet, action: Action, model: &str) -> bool {
        roles.can(&PLATFORM_APPLICATION_ROLES, action, model)
    }

    /// Rejects with `403` unless the token's roles grant the action.
    ///
    /// The mirror of [`TeamMember::require`](crate::guard::TeamMember::require),
    /// so a generated API handler authorizes with the same line its account
    /// sibling does.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, roles: &RoleSet, action: Action, model: &str) -> Result<(), ApiError> {
        if self.can(roles, action, model) {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "You do not have permission to do that.",
            ))
        }
    }
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
