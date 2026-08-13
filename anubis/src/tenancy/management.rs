//! Tenancy management: creating, renaming, and dissolving tenants.
//!
//! These routes merge into the tenancy router (conventionally under
//! `/tenancy`) and make the tenancy surface manageable rather than
//! invite-only:
//!
//! | Route | Guard | Effect |
//! |---|---|---|
//! | `POST /organizations` | signed in | Create an organization with its default team |
//! | `PATCH /organizations/{organization_id}` | org admin | Rename the organization |
//! | `DELETE /organizations/{organization_id}` | org admin | Delete the organization and everything under it |
//! | `POST /organizations/{organization_id}/teams` | org admin | Create a team, with the creator as its admin |
//! | `DELETE /organizations/{organization_id}/teams/{team_id}` | org admin | Delete a team and its records |
//! | `DELETE /organizations/{organization_id}/members/{membership_id}` | org admin | Remove an organization member |
//! | `POST /organizations/{organization_id}/leave` | org member | Leave the organization |
//! | `DELETE /organizations/{organization_id}/invitations/{invitation_id}` | org admin | Revoke any pending invitation in the organization |
//! | `PATCH /teams/{team_id}` | team admin | Rename the team |
//! | `PATCH /teams/{team_id}/members/{membership_id}` | team admin | Change a member's roles |
//! | `DELETE /teams/{team_id}/members/{membership_id}` | team admin | Remove a member |
//! | `POST /teams/{team_id}/leave` | team member | Leave the team |
//! | `DELETE /teams/{team_id}/invitations/{invitation_id}` | team admin | Revoke a pending team invitation |
//!
//! Two invariants run through all of it, and `docs/tenancy.md` states them in
//! full. A tenant always keeps at least one claimed admin, so the last one
//! cannot be demoted, removed, or walk out: those answer `409 Conflict`,
//! because the request is well formed and only the current state refuses it.
//! And deletion cascades: the framework's foreign keys, and the ones the
//! scaffolder generates, are `ON DELETE CASCADE` from `teams` and
//! `organizations`, so deleting a tenant deletes the records that chain to it.
//! An application that declares its own restricting foreign key gets a `409`
//! instead of a broken delete.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, patch, post};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::guard::{OrganizationMember, TeamMember};
use crate::http::ApiError;
use crate::schema::{
    invitations, organization_memberships, organizations, team_memberships, teams,
};
use crate::tenancy::bootstrap::{self, ADMIN_ROLE, holds_admin};
use crate::tenancy::model::{Organization, OrganizationMembership, Team, TeamMembership};
use crate::tenancy::routes::{TenancyState, log_internal, normalize_roles};

/// Longest accepted organization or team name.
const MAX_NAME_CHARS: usize = 100;

/// Returns the management routes, merged into the tenancy router.
pub(super) fn routes() -> Router<TenancyState> {
    Router::new()
        .route("/organizations", post(create_organization))
        .route(
            "/organizations/{organization_id}",
            patch(rename_organization).delete(delete_organization),
        )
        .route("/organizations/{organization_id}/teams", post(create_team))
        .route(
            "/organizations/{organization_id}/teams/{team_id}",
            delete(delete_team),
        )
        .route(
            "/organizations/{organization_id}/members/{membership_id}",
            delete(remove_organization_member),
        )
        .route(
            "/organizations/{organization_id}/leave",
            post(leave_organization),
        )
        .route(
            "/organizations/{organization_id}/invitations/{invitation_id}",
            delete(revoke_organization_invitation),
        )
        .route("/teams/{team_id}", patch(rename_team))
        .route("/teams/{team_id}/leave", post(leave_team))
        .route(
            "/teams/{team_id}/members/{membership_id}",
            patch(change_member_roles).delete(remove_member),
        )
        .route(
            "/teams/{team_id}/invitations/{invitation_id}",
            delete(revoke_team_invitation),
        )
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

#[derive(Deserialize)]
struct RolesBody {
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Serialize)]
struct OrganizationBody {
    organization: Organization,
}

#[derive(Serialize)]
struct CreatedOrganizationBody {
    organization: Organization,
    /// The default team every organization starts with.
    team: Team,
}

#[derive(Serialize)]
struct TeamBody {
    team: Team,
}

#[derive(Serialize)]
struct MembershipBody {
    membership_id: Uuid,
    roles: Vec<String>,
}

/// Creates an organization owned by the caller.
///
/// Any signed-in user may do this: an organization is how a user works with a
/// group that is not their own, and the one created at signup is not special.
async fn create_organization(
    State(state): State<TenancyState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    let name = validate_name(&body.name, "organization")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (organization, team) = connection
        .transaction(async |transaction| {
            bootstrap::create_organization(transaction, user.id, &name).await
        })
        .await
        .map_err(log_internal)?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedOrganizationBody { organization, team }),
    ))
}

async fn rename_organization(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;
    let name = validate_name(&body.name, "organization")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let organization: Organization =
        diesel::update(organizations::table.find(member.organization.id))
            .set(organizations::name.eq(&name))
            .returning(Organization::as_returning())
            .get_result(&mut connection)
            .await
            .map_err(log_internal)?;

    Ok(Json(OrganizationBody { organization }))
}

/// Deletes an organization, its teams, and everything that chains to them.
///
/// Nothing marks the organization created at signup as undeletable: its admin
/// may delete it like any other, and a user left with none creates one again
/// in a single request.
async fn delete_organization(
    State(state): State<TenancyState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::delete(organizations::table.find(member.organization.id))
        .execute(&mut connection)
        .await
        .map_err(|error| restricted_by_records(error, "organization"))?;

    Ok(StatusCode::NO_CONTENT)
}

/// Creates a team in the organization, with the creator as its admin.
async fn create_team(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;
    let name = validate_name(&body.name, "team")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let team = connection
        .transaction(async |transaction| {
            bootstrap::create_team(transaction, member.organization.id, &name, member.user.id).await
        })
        .await
        .map_err(log_internal)?;

    Ok((StatusCode::CREATED, Json(TeamBody { team })))
}

/// Deletes a team, its memberships, its invitations, and its records.
///
/// This is an organization-level act rather than a team-level one: a team's
/// own admins run the team, and dissolving it is the organization's call.
async fn delete_team(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Path((_organization_id, team_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let deleted = diesel::delete(
        teams::table
            .filter(teams::id.eq(team_id))
            .filter(teams::organization_id.eq(member.organization.id)),
    )
    .execute(&mut connection)
    .await
    .map_err(|error| restricted_by_records(error, "team"))?;

    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Removes someone else from the organization.
///
/// An organization membership carries organization-level roles, and nothing
/// else: membership in the organization's teams is a separate join, released
/// by leaving each team or by being removed from it.
async fn remove_organization_member(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Path((_organization_id, membership_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let target =
        find_organization_member(&mut connection, member.organization.id, membership_id).await?;

    if target.user_id == member.user.id {
        return Err(ApiError::validation(
            "Use the leave endpoint to leave an organization yourself.",
        ));
    }

    // The caller is an admin and is not the target, so the organization keeps
    // an admin whoever else goes.
    diesel::delete(organization_memberships::table.find(target.id))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Leaves the organization, provided the organization keeps an admin.
///
/// The teams the caller belongs to inside it are separate memberships, and
/// stay: a person can work in a team without standing in its organization,
/// which is exactly what an invitation to a single team produces.
async fn leave_organization(
    State(state): State<TenancyState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    if holds_admin(&member.membership.roles)
        && !another_organization_admin_remains(
            &mut connection,
            member.organization.id,
            member.membership.id,
        )
        .await?
    {
        return Err(last_admin_conflict("organization", "leave"));
    }

    diesel::delete(organization_memberships::table.find(member.membership.id))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn rename_team(
    State(state): State<TenancyState>,
    member: TeamMember,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;
    let name = validate_name(&body.name, "team")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let team: Team = diesel::update(teams::table.find(member.team.id))
        .set(teams::name.eq(&name))
        .returning(Team::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(TeamBody { team }))
}

/// Replaces a member's team roles with the requested set.
///
/// Roles are replaced wholesale rather than patched, so the request states the
/// end state and two admins editing the same member cannot interleave into a
/// set neither asked for.
async fn change_member_roles(
    State(state): State<TenancyState>,
    member: TeamMember,
    Path((_team_id, membership_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RolesBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;
    let roles = normalize_roles(&state.roles, body.roles)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let target = find_member(&mut connection, member.team.id, membership_id).await?;

    if target.user_id.is_some()
        && holds_admin(&target.roles)
        && !holds_admin(&roles)
        && !another_admin_remains(&mut connection, member.team.id, target.id).await?
    {
        return Err(last_admin_conflict("team", "step down"));
    }

    connection
        .transaction(async |transaction| {
            diesel::update(team_memberships::table.find(target.id))
                .set(team_memberships::roles.eq(&roles))
                .execute(transaction)
                .await?;
            // A pending member's invitation carries a copy of the roles; keep
            // the record honest even though the claim adopts the membership.
            diesel::update(invitations::table.filter(invitations::team_membership_id.eq(target.id)))
                .set(invitations::roles.eq(&roles))
                .execute(transaction)
                .await
        })
        .await
        .map_err(log_internal)?;

    Ok(Json(MembershipBody {
        membership_id: target.id,
        roles,
    }))
}

/// Removes someone else from the team.
///
/// Removing a pending member cancels their invitation with it, since the
/// invitation's row cascades from the membership it pre-created.
async fn remove_member(
    State(state): State<TenancyState>,
    member: TeamMember,
    Path((_team_id, membership_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let target = find_member(&mut connection, member.team.id, membership_id).await?;

    if target.user_id == Some(member.user.id) {
        return Err(ApiError::validation(
            "Use the leave endpoint to leave a team yourself.",
        ));
    }

    // The caller is a claimed admin and is not the target, so the team keeps
    // an admin whoever else goes.
    diesel::delete(team_memberships::table.find(target.id))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Leaves the team, provided the team keeps an admin.
async fn leave_team(
    State(state): State<TenancyState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    if holds_admin(&member.membership.roles)
        && !another_admin_remains(&mut connection, member.team.id, member.membership.id).await?
    {
        return Err(last_admin_conflict("team", "leave"));
    }

    diesel::delete(team_memberships::table.find(member.membership.id))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_team_invitation(
    State(state): State<TenancyState>,
    member: TeamMember,
    Path((_team_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let pending_membership = diesel::delete(
        invitations::table
            .filter(invitations::id.eq(invitation_id))
            .filter(invitations::team_id.eq(member.team.id)),
    )
    .returning(invitations::team_membership_id)
    .get_result::<Option<Uuid>>(&mut connection)
    .await
    .optional()
    .map_err(log_internal)?
    .ok_or_else(ApiError::not_found)?;

    discard_pending_membership(&mut connection, pending_membership).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Revokes any pending invitation in the organization, team ones included.
///
/// Organization admins may invite into any team of their organization, so they
/// may take those invitations back.
async fn revoke_organization_invitation(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Path((_organization_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_organization_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let pending_membership = diesel::delete(
        invitations::table
            .filter(invitations::id.eq(invitation_id))
            .filter(invitations::organization_id.eq(member.organization.id)),
    )
    .returning(invitations::team_membership_id)
    .get_result::<Option<Uuid>>(&mut connection)
    .await
    .optional()
    .map_err(log_internal)?
    .ok_or_else(ApiError::not_found)?;

    discard_pending_membership(&mut connection, pending_membership).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the unclaimed membership a revoked team invitation pre-created.
///
/// Organization invitations create no membership and pass `None`. A claimed
/// invitation no longer exists at all, so claiming and revoking race to the
/// same honest answer: `404` for whoever arrives second.
async fn discard_pending_membership(
    connection: &mut AsyncPgConnection,
    membership_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(membership_id) = membership_id else {
        return Ok(());
    };

    diesel::delete(
        team_memberships::table
            .filter(team_memberships::id.eq(membership_id))
            .filter(team_memberships::user_id.is_null()),
    )
    .execute(connection)
    .await
    .map_err(log_internal)?;

    Ok(())
}

/// Loads one membership of the team, answering `404` for anything else.
async fn find_member(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    membership_id: Uuid,
) -> Result<TeamMembership, ApiError> {
    team_memberships::table
        .filter(team_memberships::id.eq(membership_id))
        .filter(team_memberships::team_id.eq(team_id))
        .select(TeamMembership::as_select())
        .first(connection)
        .await
        .optional()
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads one membership of the organization, answering `404` for anything else.
async fn find_organization_member(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    membership_id: Uuid,
) -> Result<OrganizationMembership, ApiError> {
    organization_memberships::table
        .filter(organization_memberships::id.eq(membership_id))
        .filter(organization_memberships::organization_id.eq(organization_id))
        .select(OrganizationMembership::as_select())
        .first(connection)
        .await
        .optional()
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Returns `true` when the team has a claimed admin other than `excluded`.
///
/// Unclaimed memberships are invitations, not people, so they never count.
async fn another_admin_remains(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    excluded: Uuid,
) -> Result<bool, ApiError> {
    let admins: i64 = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::id.ne(excluded))
        .filter(team_memberships::user_id.is_not_null())
        .filter(team_memberships::roles.contains(vec![ADMIN_ROLE]))
        .count()
        .get_result(connection)
        .await
        .map_err(log_internal)?;

    Ok(admins > 0)
}

/// Returns `true` when the organization has an admin other than `excluded`.
///
/// Organization memberships exist only once claimed, so every one of them
/// counts.
async fn another_organization_admin_remains(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    excluded: Uuid,
) -> Result<bool, ApiError> {
    let admins: i64 = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::id.ne(excluded))
        .filter(organization_memberships::roles.contains(vec![ADMIN_ROLE]))
        .count()
        .get_result(connection)
        .await
        .map_err(log_internal)?;

    Ok(admins > 0)
}

/// Trims and bounds a submitted display name.
fn validate_name(raw: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::validation(format!(
            "Give the {label} a name of 1 to {MAX_NAME_CHARS} characters."
        )));
    }
    Ok(trimmed.to_owned())
}

fn require_team_admin(member: &TeamMember) -> Result<(), ApiError> {
    if holds_admin(&member.membership.roles) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need the admin role to manage this team.",
        ))
    }
}

fn require_organization_admin(member: &OrganizationMember) -> Result<(), ApiError> {
    if holds_admin(&member.membership.roles) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need the admin role to manage this organization.",
        ))
    }
}

/// The refusal that keeps every tenant administrable.
///
/// Only the last admin acting on themselves can reach it: an admin editing
/// somebody else still counts as the admin the tenant is left with.
fn last_admin_conflict(tenant: &str, attempt: &str) -> ApiError {
    ApiError::conflict(format!(
        "A {tenant} needs at least one admin. \
         Give someone else the admin role before you {attempt}."
    ))
}

/// Maps a restricting foreign key into a conflict instead of a 500.
///
/// The framework's tables and the scaffolder's output cascade from `teams` and
/// `organizations`, so this only fires for an application that deliberately
/// declared a restricting reference of its own; the caller's remedy is to
/// remove those records first.
fn restricted_by_records(error: diesel::result::Error, label: &str) -> ApiError {
    match error {
        diesel::result::Error::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _details) => {
            ApiError::conflict(format!(
                "This {label} still owns records that have to be removed first."
            ))
        }
        other => log_internal(other),
    }
}
