//! Tenancy endpoints: inviting members and claiming invitations.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/tenancy`): `POST /invitations` sends an invitation to a team or an
//! organization, `POST /invitations/claim` joins the signed-in user to the
//! invitation's target. Inviting requires the admin role on the target;
//! organization admins may invite to any team in their organization.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::mail::{Email, Mailer};
use crate::roles::RoleSet;
use crate::schema::{organization_memberships, organizations, team_memberships, teams};
use crate::tenancy::bootstrap::ADMIN_ROLE;
use crate::tenancy::invitation::{self, InvitationTarget};
use crate::tenancy::model::{Organization, Team};

/// Returns the tenancy routes for an application to mount.
///
/// The role set validates requested role keys; the mailer delivers
/// invitation email; the config supplies the public base URL for links.
pub fn router(pool: DbPool, mailer: Mailer, roles: RoleSet, config: &AppConfig) -> Router {
    Router::new()
        .route("/invitations", post(create_invitation))
        .route("/invitations/claim", post(claim_invitation))
        .with_state(TenancyState {
            pool: pool.clone(),
            mailer,
            roles,
            app_url: config.app_url.clone(),
        })
        // CurrentUser resolves its pool from request extensions.
        .layer(Extension(pool))
}

#[derive(Clone)]
struct TenancyState {
    pool: DbPool,
    mailer: Mailer,
    roles: RoleSet,
    app_url: String,
}

#[derive(Deserialize)]
struct CreateInvitationBody {
    email: String,
    /// Exactly one of `team_id` or `organization_id` must be set.
    team_id: Option<Uuid>,
    organization_id: Option<Uuid>,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Serialize)]
struct InvitationResponse {
    id: Uuid,
    email: String,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct InvitationBody {
    invitation: InvitationResponse,
}

#[derive(Deserialize)]
struct ClaimBody {
    token: String,
}

#[derive(Serialize)]
struct ClaimResponseBody {
    organization: Organization,
    team: Option<Team>,
}

async fn create_invitation(
    State(state): State<TenancyState>,
    CurrentUser(inviter): CurrentUser,
    Json(body): Json<CreateInvitationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::validation("Enter a valid email address."));
    }

    let granted_roles = if body.roles.is_empty() {
        vec!["default".to_owned()]
    } else {
        body.roles.clone()
    };
    for role in &granted_roles {
        if !state.roles.is_defined(role) {
            return Err(ApiError::validation(format!(
                "Unknown role {role:?}. Define it in config/roles.yml first."
            )));
        }
    }

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (target, target_name) =
        resolve_invitation_target(&mut connection, &body, inviter.id).await?;

    let token = connection
        .transaction(async |transaction| {
            invitation::create(transaction, target, &email, &granted_roles, inviter.id).await
        })
        .await
        .map_err(log_internal)?;

    let link = format!("{}/claim-invitation?token={token}", state.app_url);
    let mail = Email {
        to: email.clone(),
        subject: format!("You're invited to join {target_name}"),
        text_body: format!(
            "{} invited you to join {target_name}.\n\n\
             Accept within {} days: {link}\n\n\
             If you weren't expecting this, ignore this email.",
            inviter.email,
            invitation::INVITATION_TTL_DAYS,
        ),
    };
    if let Err(error) = state.mailer.send(mail).await {
        tracing::error!(
            error.message = %error,
            "failed to deliver the invitation email: {{error.message}}",
        );
    }

    // Re-read what was stored so the response reflects the database.
    let stored: (Uuid, DateTime<Utc>) = crate::schema::invitations::table
        .filter(crate::schema::invitations::email.eq(&email))
        .order(crate::schema::invitations::created_at.desc())
        .select((
            crate::schema::invitations::id,
            crate::schema::invitations::expires_at,
        ))
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    let response = InvitationBody {
        invitation: InvitationResponse {
            id: stored.0,
            email,
            expires_at: stored.1,
        },
    };
    Ok((StatusCode::CREATED, Json(response)))
}

async fn claim_invitation(
    State(state): State<TenancyState>,
    CurrentUser(claimant): CurrentUser,
    Json(body): Json<ClaimBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let claimed = connection
        .transaction(async |transaction| {
            invitation::claim(transaction, &body.token, claimant.id).await
        })
        .await
        .map_err(log_internal)?
        .ok_or_else(|| {
            ApiError::validation("That invitation is invalid or has expired. Ask for a new one.")
        })?;

    let organization: Organization = organizations::table
        .find(claimed.organization_id)
        .select(Organization::as_select())
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    let team: Option<Team> = match claimed.team_id {
        None => None,
        Some(team_id) => teams::table
            .find(team_id)
            .select(Team::as_select())
            .first(&mut connection)
            .await
            .optional()
            .map_err(log_internal)?,
    };

    Ok(Json(ClaimResponseBody { organization, team }))
}

/// Resolves the invitation target, authorizes the inviter against it, and
/// returns the target's display name for the email.
async fn resolve_invitation_target(
    connection: &mut AsyncPgConnection,
    body: &CreateInvitationBody,
    inviter_id: Uuid,
) -> Result<(InvitationTarget, String), ApiError> {
    match (body.team_id, body.organization_id) {
        (Some(team_id), None) => {
            let team: Team = teams::table
                .find(team_id)
                .select(Team::as_select())
                .first(connection)
                .await
                .optional()
                .map_err(log_internal)?
                .ok_or_else(|| ApiError::validation("That team does not exist."))?;

            let team_admin = holds_admin_on_team(connection, team.id, inviter_id)
                .await
                .map_err(log_internal)?;
            let org_admin =
                holds_admin_on_organization(connection, team.organization_id, inviter_id)
                    .await
                    .map_err(log_internal)?;
            if !team_admin && !org_admin {
                return Err(not_allowed());
            }

            Ok((
                InvitationTarget::Team {
                    team_id: team.id,
                    organization_id: team.organization_id,
                },
                team.name,
            ))
        }
        (None, Some(organization_id)) => {
            let organization: Organization = organizations::table
                .find(organization_id)
                .select(Organization::as_select())
                .first(connection)
                .await
                .optional()
                .map_err(log_internal)?
                .ok_or_else(|| ApiError::validation("That organization does not exist."))?;

            let org_admin = holds_admin_on_organization(connection, organization.id, inviter_id)
                .await
                .map_err(log_internal)?;
            if !org_admin {
                return Err(not_allowed());
            }

            Ok((
                InvitationTarget::Organization(organization.id),
                organization.name,
            ))
        }
        _other => Err(ApiError::validation(
            "Set exactly one of team_id or organization_id.",
        )),
    }
}

async fn holds_admin_on_organization(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let roles: Option<Vec<String>> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::user_id.eq(user_id))
        .select(organization_memberships::roles)
        .first(connection)
        .await
        .optional()?;

    Ok(roles.is_some_and(|held| held.iter().any(|role| role == ADMIN_ROLE)))
}

async fn holds_admin_on_team(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let roles: Option<Vec<String>> = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.eq(user_id))
        .select(team_memberships::roles)
        .first(connection)
        .await
        .optional()?;

    Ok(roles.is_some_and(|held| held.iter().any(|role| role == ADMIN_ROLE)))
}

fn not_allowed() -> ApiError {
    ApiError::forbidden("You need the admin role to invite members.")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "tenancy request failed: {{error.message}}",
    );
    ApiError::internal()
}
