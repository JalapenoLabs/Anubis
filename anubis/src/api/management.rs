//! Platform application management: the "Developers" section endpoints.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/developers`). All routes are team-scoped through the [`TeamMember`]
//! guard and additionally require the admin role key, since a platform token
//! acts as the whole team.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::platform::{self, PlatformApplication};
use crate::db::DbPool;
use crate::guard::TeamMember;
use crate::http::ApiError;
use crate::roles::RoleSet;
use crate::schema::platform_applications;
use crate::tenancy::ADMIN_ROLE;

/// Returns the platform application management routes.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/teams/{team_id}/platform-applications",
            get(list_applications).post(create_application),
        )
        .route(
            "/teams/{team_id}/platform-applications/{application_id}",
            axum::routing::delete(delete_application),
        )
        .route(
            "/teams/{team_id}/platform-applications/{application_id}/rotate-token",
            post(rotate_token),
        )
        .with_state(ManagementState { pool: pool.clone() })
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
struct ManagementState {
    pool: DbPool,
}

#[derive(Deserialize)]
struct CreateApplicationBody {
    name: String,
}

#[derive(Serialize)]
struct ApplicationsBody {
    applications: Vec<PlatformApplication>,
}

#[derive(Serialize)]
struct CreatedApplicationBody {
    application: PlatformApplication,
    /// Shown exactly once; only its hash is stored.
    token: String,
}

#[derive(Serialize)]
struct RotatedTokenBody {
    /// Shown exactly once; every previous token is now dead.
    token: String,
}

async fn list_applications(
    State(state): State<ManagementState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let applications: Vec<PlatformApplication> = platform_applications::table
        .filter(platform_applications::team_id.eq(member.team.id))
        .select(PlatformApplication::as_select())
        .order(platform_applications::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(ApplicationsBody { applications }))
}

async fn create_application(
    State(state): State<ManagementState>,
    member: TeamMember,
    Json(body): Json<CreateApplicationBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("Name the application."));
    }

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (application, raw_token) = connection
        .transaction(async |transaction| platform::create(transaction, member.team.id, name).await)
        .await
        .map_err(log_internal)?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedApplicationBody {
            application,
            token: raw_token,
        }),
    ))
}

async fn delete_application(
    State(state): State<ManagementState>,
    member: TeamMember,
    axum::extract::Path((_team_id, application_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let deleted = diesel::delete(
        platform_applications::table
            .filter(platform_applications::id.eq(application_id))
            .filter(platform_applications::team_id.eq(member.team.id)),
    )
    .execute(&mut connection)
    .await
    .map_err(log_internal)?;

    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_token(
    State(state): State<ManagementState>,
    member: TeamMember,
    axum::extract::Path((_team_id, application_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    // Scope the rotation to the member's team before touching tokens.
    let owned: Option<Uuid> = platform_applications::table
        .filter(platform_applications::id.eq(application_id))
        .filter(platform_applications::team_id.eq(member.team.id))
        .select(platform_applications::id)
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;
    let Some(application_id) = owned else {
        return Err(ApiError::not_found());
    };

    let raw_token = connection
        .transaction(async |transaction| platform::rotate_token(transaction, application_id).await)
        .await
        .map_err(log_internal)?;

    Ok(Json(RotatedTokenBody { token: raw_token }))
}

/// Platform tokens act as the whole team, so management needs the admin key.
fn require_team_admin(member: &TeamMember) -> Result<(), ApiError> {
    if member
        .membership
        .roles
        .iter()
        .any(|role| role == ADMIN_ROLE)
    {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need the admin role to manage platform applications.",
        ))
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "platform application request failed: {{error.message}}",
    );
    ApiError::internal()
}
