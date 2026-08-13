//! Tenancy endpoints: membership overview, invitations, and management.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/tenancy`). This module serves the read and invitation side:
//! `GET /memberships` lists what the caller belongs to, `GET
//! /teams/{team_id}/members` is the team roster, `POST /invitations` sends an
//! invitation to a team or an organization, and `POST /invitations/claim`
//! joins the signed-in user to the invitation's target. Inviting requires the
//! admin role on the target; organization admins may invite to any team in
//! their organization.
//!
//! The management routes, which create, rename, and dissolve tenants and move
//! members between roles, live in [`super::management`] and merge in here.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::TeamMember;
use crate::http::ApiError;
use crate::mail::{Email, Mailer};
use crate::roles::RoleSet;
use crate::schema::{
    invitations, organization_memberships, organizations, team_memberships, teams, users,
};
use crate::tenancy::bootstrap::{DEFAULT_ROLE, holds_admin};
use crate::tenancy::invitation::{self, InvitationTarget};
use crate::tenancy::model::{Organization, Team};

/// Returns the tenancy routes for an application to mount.
///
/// The role set validates requested role keys; the mailer delivers
/// invitation email; the config supplies the public base URL for links.
pub fn router(pool: DbPool, mailer: Mailer, roles: RoleSet, config: &AppConfig) -> Router {
    Router::new()
        .route("/memberships", get(list_memberships))
        .route("/teams/{team_id}/members", get(list_team_members))
        .route("/invitations", post(create_invitation))
        .route("/invitations/claim", post(claim_invitation))
        .merge(super::management::routes())
        .with_state(TenancyState {
            pool: pool.clone(),
            mailer,
            roles: roles.clone(),
            app_url: config.app_url.clone(),
        })
        // CurrentUser and the guard extractors resolve their dependencies
        // from these request extensions.
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
pub(super) struct TenancyState {
    pub(super) pool: DbPool,
    pub(super) roles: RoleSet,
    mailer: Mailer,
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

#[derive(Serialize)]
struct MembershipTeam {
    id: Uuid,
    name: String,
    roles: Vec<String>,
}

#[derive(Serialize)]
struct MembershipOrganization {
    id: Uuid,
    name: String,
    /// Organization-level roles; empty for users who only belong to teams.
    roles: Vec<String>,
    teams: Vec<MembershipTeam>,
}

#[derive(Serialize)]
struct MembershipsBody {
    organizations: Vec<MembershipOrganization>,
}

/// Everything the signed-in user belongs to, grouped by organization.
///
/// Users can hold team memberships without an organization membership (they
/// were invited to a team only), so organizations are collected from both
/// membership tables.
async fn list_memberships(
    State(state): State<TenancyState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let team_rows: Vec<(Organization, Team, Vec<String>)> = team_memberships::table
        .inner_join(teams::table.inner_join(organizations::table))
        .filter(team_memberships::user_id.eq(user.id))
        .select((
            Organization::as_select(),
            Team::as_select(),
            team_memberships::roles,
        ))
        .order((organizations::name.asc(), teams::name.asc()))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let org_rows: Vec<(Organization, Vec<String>)> = organization_memberships::table
        .inner_join(organizations::table)
        .filter(organization_memberships::user_id.eq(user.id))
        .select((Organization::as_select(), organization_memberships::roles))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut grouped: BTreeMap<Uuid, MembershipOrganization> = BTreeMap::new();
    for (organization, roles) in org_rows {
        grouped.insert(
            organization.id,
            MembershipOrganization {
                id: organization.id,
                name: organization.name,
                roles,
                teams: Vec::new(),
            },
        );
    }
    for (organization, team, roles) in team_rows {
        let entry = grouped
            .entry(organization.id)
            .or_insert(MembershipOrganization {
                id: organization.id,
                name: organization.name,
                roles: Vec::new(),
                teams: Vec::new(),
            });
        entry.teams.push(MembershipTeam {
            id: team.id,
            name: team.name,
            roles,
        });
    }

    let mut organizations_list: Vec<MembershipOrganization> = grouped.into_values().collect();
    organizations_list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

    Ok(Json(MembershipsBody {
        organizations: organizations_list,
    }))
}

#[derive(Serialize)]
struct TeamMemberEntry {
    membership_id: Uuid,
    /// The member's email, from the account or the pending invitation.
    email: Option<String>,
    roles: Vec<String>,
    /// True for invited members who have not claimed their membership yet.
    pending: bool,
    /// The invitation to revoke, for a pending member.
    invitation_id: Option<Uuid>,
}

#[derive(Serialize)]
struct TeamMembersBody {
    members: Vec<TeamMemberEntry>,
}

/// One roster row: membership id, roles, account email, invitation, its email.
type RosterRow = (
    Uuid,
    Vec<String>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
);

/// The team's roster, visible to any member of the team.
async fn list_team_members(
    State(state): State<TenancyState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let rows: Vec<RosterRow> = team_memberships::table
        .left_join(users::table)
        .left_join(
            invitations::table
                .on(invitations::team_membership_id.eq(team_memberships::id.nullable())),
        )
        .filter(team_memberships::team_id.eq(member.team.id))
        .select((
            team_memberships::id,
            team_memberships::roles,
            users::email.nullable(),
            invitations::id.nullable(),
            invitations::email.nullable(),
        ))
        .order(team_memberships::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let members = rows
        .into_iter()
        .map(
            |(membership_id, roles, user_email, invitation_id, invited_email)| TeamMemberEntry {
                membership_id,
                pending: user_email.is_none(),
                email: user_email.or(invited_email),
                roles,
                invitation_id,
            },
        )
        .collect();

    Ok(Json(TeamMembersBody { members }))
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

    let granted_roles = normalize_roles(&state.roles, body.roles.clone())?;

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

    Ok(roles.is_some_and(|held| holds_admin(&held)))
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

    Ok(roles.is_some_and(|held| holds_admin(&held)))
}

fn not_allowed() -> ApiError {
    ApiError::forbidden("You need the admin role to invite members.")
}

/// Validates requested role keys against the application's role set.
///
/// An empty list means the baseline role, so a caller who does not care about
/// roles still lands somewhere defined rather than with none at all.
pub(super) fn normalize_roles(
    roles: &RoleSet,
    requested: Vec<String>,
) -> Result<Vec<String>, ApiError> {
    let granted = if requested.is_empty() {
        vec![DEFAULT_ROLE.to_owned()]
    } else {
        requested
    };

    for role in &granted {
        if !roles.is_defined(role) {
            return Err(ApiError::validation(format!(
                "Unknown role {role:?}. Define it in config/roles.yml first."
            )));
        }
    }

    Ok(granted)
}

pub(super) fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "tenancy request failed: {{error.message}}",
    );
    ApiError::internal()
}
