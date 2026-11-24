//! Tenancy management: creating, renaming, and dissolving tenants, and
//! granting and revoking members' roles.
//!
//! These routes merge into the tenancy router (conventionally under
//! `/tenancy`) and make the tenancy surface manageable rather than
//! invite-only:
//!
//! | Route | Asks for | Effect |
//! |---|---|---|
//! | `POST /organizations` | signed in | Create an organization with its default team |
//! | `PATCH /organizations/{organization_id}` | administrator | Rename the organization |
//! | `DELETE /organizations/{organization_id}` | administrator | Delete the organization and everything under it |
//! | `POST /organizations/{organization_id}/teams` | administrator | Create a team, with the creator as its administrator |
//! | `DELETE /organizations/{organization_id}/teams/{team_id}` | administrator | Delete a team and its records |
//! | `PATCH /organizations/{organization_id}/members/{membership_id}` | `Tenancy::Membership.update` | Replace a member's organization-wide roles |
//! | `DELETE /organizations/{organization_id}/members/{membership_id}` | `Tenancy::Membership.destroy` | Remove an organization member |
//! | `POST /organizations/{organization_id}/leave` | member | Leave the organization |
//! | `DELETE /organizations/{organization_id}/invitations/{invitation_id}` | `Tenancy::Membership.destroy` | Revoke any pending invitation in the organization |
//! | `PATCH /teams/{team_id}` | team administrator | Rename the team |
//! | `PATCH /teams/{team_id}/members/{membership_id}` | `Tenancy::Membership.update` | Replace a member's roles in the team, or on one record of it |
//! | `DELETE /teams/{team_id}/members/{membership_id}` | `Tenancy::Membership.destroy` | Remove a member |
//! | `POST /teams/{team_id}/leave` | member | Leave the team |
//! | `DELETE /teams/{team_id}/invitations/{invitation_id}` | `Tenancy::Membership.destroy` | Revoke a pending team invitation |
//!
//! Every grant, revocation, and removal is also held to the role hierarchy
//! (see [`super::hierarchy`]): nobody acts on a role or a member at or above
//! their own rank, and nobody hands out a permission they do not hold.
//!
//! Two invariants run through all of it, and `docs/tenancy.md` states them in
//! full.
//!
//! A tenant always keeps at least one claimed administrator, so the last one
//! cannot be demoted, removed, or walk out: those answer `409 Conflict`,
//! because the request is well formed and only the current state refuses it.
//! Every membership and role change runs inside a transaction that locks the
//! organization's row first ([`lock_organization`], and [`lock_team`] after it
//! for a team), resolves the caller's standing again under that lock, and
//! counts the administrators left before committing. Ordering is what makes
//! the count mean anything: two administrators acting at the same instant
//! would otherwise each read the other as the one who remains, and both would
//! commit.
//!
//! Deletion cascades: the framework's foreign keys, and the ones the
//! scaffolder generates, are `ON DELETE CASCADE` from `teams` and
//! `organizations`, so deleting a tenant deletes the records that chain to it.
//! An application that declares its own restricting foreign key gets a `409`
//! instead of a broken delete.

use std::collections::BTreeSet;

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

use crate::audit::{self, Changes};
use crate::auth::{CurrentUser, User};
use crate::guard::{OrganizationMember, TeamMember};
use crate::http::ApiError;
use crate::roles::{Action, MEMBERSHIP, RoleSet, Scope};
use crate::schema::{
    invitations, organization_memberships, organizations, team_memberships, teams, users,
};
use crate::tenancy::access::{
    Access, organization_membership_access, resolve_organization_access, resolve_team_access,
    team_membership_access,
};
use crate::tenancy::bootstrap;
use crate::tenancy::grants::{self, Holder};
use crate::tenancy::hierarchy;
use crate::tenancy::invitation;
use crate::tenancy::model::{Organization, OrganizationMembership, Team, TeamMembership};
use crate::tenancy::role::{Role, RoleSummary};
use crate::tenancy::routes::{TenancyState, load_roles, log_internal, role_names};

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
            patch(change_organization_member_roles).delete(remove_organization_member),
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

/// Replaces roles wholesale, so it is strict: a body that does not name the
/// roles, or names a field this server does not take, is refused rather than
/// read as "none", which would revoke everything at that scope.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesBody {
    /// The roles the member is to hold at this scope, replacing what they
    /// held there. Empty leaves them with the default role alone.
    role_ids: Vec<Uuid>,
    /// Narrows the grant to one record of the team; absent means team-wide.
    #[serde(default)]
    resource_id: Option<Uuid>,
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
    /// The roles now held at the scope the request named, highest first.
    roles: Vec<RoleSummary>,
}

/// Creates an organization owned by the caller.
///
/// Any signed-in user may do this: an organization is how a user works with a
/// group that is not their own, and the one created at signup is not special.
/// It starts with the whole recommended set of roles, because this surface has
/// the application's `roles.yml` in hand.
async fn create_organization(
    State(state): State<TenancyState>,
    CurrentUser(user): CurrentUser,
    context: audit::Context,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    let name = validate_name(&body.name, "organization")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (organization, team) = connection
        .transaction::<(Organization, Team), diesel::result::Error, _>(async |transaction| {
            let (organization, team) =
                bootstrap::create_organization(transaction, user.id, &name).await?;
            super::role::offer_recommended(transaction, &state.roles, organization.id).await?;
            audit::record(
                transaction,
                &context.by(&user),
                &audit::Event::new(audit::ORGANIZATION_CREATED, "Organization")
                    .organization(organization.id)
                    .subject(organization.id)
                    .label(&organization.name),
            )
            .await?;
            Ok((organization, team))
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
    context: audit::Context,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    member
        .access
        .require_administrator("rename this organization")?;
    let name = validate_name(&body.name, "organization")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // The rename and its record share one transaction, so the log can never
    // hold a rename that did not happen, nor miss one that did.
    let organization: Organization = connection
        .transaction::<Organization, diesel::result::Error, _>(async |transaction| {
            let organization: Organization =
                diesel::update(organizations::table.find(member.organization.id))
                    .set(organizations::name.eq(&name))
                    .returning(Organization::as_returning())
                    .get_result(transaction)
                    .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::ORGANIZATION_RENAMED, "Organization")
                    .organization(organization.id)
                    .subject(organization.id)
                    .label(&organization.name)
                    .changes(Changes::new().field(
                        "name",
                        member.organization.name.clone(),
                        organization.name.clone(),
                    )),
            )
            .await?;

            Ok(organization)
        })
        .await
        .map_err(log_internal)?;

    Ok(Json(OrganizationBody { organization }))
}

/// Deletes an organization, its teams, and everything that chains to them.
///
/// Nothing marks the organization created at signup as undeletable: its
/// administrator may delete it like any other, and a user left with none
/// creates one again in a single request.
async fn delete_organization(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
) -> Result<impl IntoResponse, ApiError> {
    member
        .access
        .require_administrator("delete this organization")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::delete(organizations::table.find(member.organization.id))
        .execute(&mut connection)
        .await
        .map_err(|error| restricted_by_records(error, "organization"))?;

    // Recorded against nobody's tenant, deliberately. Audit rows cascade from
    // the organization they name, so naming this one would delete the record
    // of its own deletion; the act lands in the actor's account log instead,
    // which is the one place that outlives the organization.
    audit::record(
        &mut connection,
        &context.by(&member.user),
        &audit::Event::new(audit::ORGANIZATION_DESTROYED, "Organization")
            .subject(member.organization.id)
            .label(&member.organization.name),
    )
    .await
    .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Creates a team in the organization, with the creator as its administrator.
async fn create_team(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    member.access.require_administrator("create teams")?;
    let name = validate_name(&body.name, "team")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let team = connection
        .transaction::<Team, diesel::result::Error, _>(async |transaction| {
            let team =
                // Organization-level, so the team is inherited by every
                // sub-tenant: scoping one is a follow-up endpoint's act.
                bootstrap::create_team(
                    transaction,
                    member.organization.id,
                    &name,
                    None,
                    member.user.id,
                )
                    .await?;
            // Recorded against the new team rather than its organization, so
            // the first line of a team's own log says where the team came from.
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::TEAM_CREATED, "Team")
                    .team(team.id)
                    .subject(team.id)
                    .label(&team.name),
            )
            .await?;
            Ok(team)
        })
        .await
        .map_err(log_internal)?;

    Ok((StatusCode::CREATED, Json(TeamBody { team })))
}

/// Deletes a team, its memberships, its invitations, and its records.
///
/// This is an organization-level act rather than a team-level one: a team's
/// own administrators run the team, and dissolving it is the organization's
/// call.
async fn delete_team(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, team_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    member.access.require_administrator("delete teams")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // The name comes back with the delete, because the audit event has to say
    // which team went and by then there is nothing left to ask.
    let deleted: Option<String> = diesel::delete(
        teams::table
            .filter(teams::id.eq(team_id))
            .filter(teams::organization_id.eq(member.organization.id)),
    )
    .returning(teams::name)
    .get_result(&mut connection)
    .await
    .optional()
    .map_err(|error| restricted_by_records(error, "team"))?;

    let Some(name) = deleted else {
        return Err(ApiError::not_found());
    };

    // Recorded against the organization: the team's own audit rows cascaded
    // away with it, and the organization is what survives to hold this one.
    audit::record(
        &mut connection,
        &context.by(&member.user),
        &audit::Event::new(audit::TEAM_DESTROYED, "Team")
            .organization(member.organization.id)
            .subject(team_id)
            .label(&name),
    )
    .await
    .map_err(log_internal)?;

    // The team's memberships went with it, and some of those people may have
    // held a seat nowhere else.
    state
        .queue_seat_sync(&mut connection, member.organization.id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replaces a member's organization-wide roles with the requested set.
///
/// Roles are replaced wholesale rather than patched, so the request states the
/// end state and two managers editing the same member cannot interleave into
/// a set neither asked for.
async fn change_organization_member_roles(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, membership_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RolesBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.resource_id.is_some() {
        return Err(ApiError::validation(
            "Grants on a single record belong to the team that owns it.",
        ));
    }
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let held = connection
        .transaction::<Vec<Role>, ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Update, MEMBERSHIP)?;

            let target =
                find_organization_member(transaction, organization_id, membership_id).await?;
            let target_access = organization_membership_access(
                transaction,
                &state.roles,
                organization_id,
                target.id,
            )
            .await?;
            hierarchy::require_outranks_member(
                &actor,
                &target_access,
                target.user_id == member.user.id,
            )?;

            let holder = Holder::Organization {
                membership_id: target.id,
            };
            let change = replace_grants(
                transaction,
                &state.roles,
                &actor,
                organization_id,
                holder,
                Scope::Organization,
                &body.role_ids,
                member.user.id,
            )
            .await?;

            if change.revoked_administrator {
                require_organization_keeps_an_admin(transaction, organization_id, "step down")
                    .await?;
            }

            if change.is_noop() {
                return Ok(change.after);
            }

            let label = account_label(transaction, target.user_id).await?;
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_ROLE_CHANGED, "OrganizationMembership")
                    .organization(organization_id)
                    .subject(target.id)
                    .label(&label)
                    .changes(change.changes(&state.roles)),
            )
            .await?;

            if target.user_id != member.user.id {
                notify_roles_changed(
                    transaction,
                    &state.roles,
                    target.user_id,
                    None,
                    &member.organization.name,
                    &change.after,
                    &crate::notifications::organization_settings_href(organization_id),
                )
                .await?;
            }

            Ok(change.after)
        })
        .await?;

    Ok(Json(MembershipBody {
        membership_id,
        roles: held.iter().map(|role| role.summary(&state.roles)).collect(),
    }))
}

/// Removes someone else from the organization.
///
/// An organization membership carries organization-wide roles, and nothing
/// else: membership in the organization's teams is a separate join, released
/// by leaving each team or by being removed from it.
async fn remove_organization_member(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, membership_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Destroy, MEMBERSHIP)?;

            let target =
                find_organization_member(transaction, organization_id, membership_id).await?;
            if target.user_id == member.user.id {
                return Err(ApiError::validation(
                    "Use the leave endpoint to leave an organization yourself.",
                ));
            }
            let target_access = organization_membership_access(
                transaction,
                &state.roles,
                organization_id,
                target.id,
            )
            .await?;
            hierarchy::require_outranks_member(&actor, &target_access, false)?;

            // Read before the delete, because the label has to say who left
            // and afterwards the membership is gone.
            let label = account_label(transaction, target.user_id).await?;

            // Its grants cascade with it.
            diesel::delete(organization_memberships::table.find(target.id))
                .execute(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_REMOVED, "OrganizationMembership")
                    .organization(organization_id)
                    .subject(target.id)
                    .label(&label)
                    .changes(Changes::new().field(
                        "roles",
                        held_names(&target_access),
                        Vec::<String>::new(),
                    )),
            )
            .await?;

            // Two administrators removing each other at the same instant
            // would each leave the other standing on their own reading; the
            // lock orders them and this count catches whichever arrives
            // second.
            if target_access.is_administrator() {
                require_organization_keeps_an_admin(transaction, organization_id, "remove them")
                    .await?;
            }
            state.queue_seat_sync(transaction, organization_id).await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Leaves the organization, provided the organization keeps an administrator.
///
/// The teams the caller belongs to inside it are separate memberships, and
/// stay: a person can work in a team without standing in its organization,
/// which is exactly what an invitation to a single team produces.
async fn leave_organization(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            // Re-read under the lock: a request that committed while this one
            // was in flight may already have taken the caller's administrator
            // role, in which case they are free to go.
            let leaving =
                find_organization_member(transaction, organization_id, member.membership.id)
                    .await?;
            let leaving_access = organization_membership_access(
                transaction,
                &state.roles,
                organization_id,
                leaving.id,
            )
            .await?;

            diesel::delete(organization_memberships::table.find(leaving.id))
                .execute(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_LEFT, "OrganizationMembership")
                    .organization(organization_id)
                    .subject(leaving.id)
                    .label(&audit::person_label(
                        member.user.first_name.as_deref(),
                        member.user.last_name.as_deref(),
                        &member.user.email,
                    )),
            )
            .await?;

            if leaving_access.is_administrator() {
                require_organization_keeps_an_admin(transaction, organization_id, "leave").await?;
            }
            state.queue_seat_sync(transaction, organization_id).await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn rename_team(
    State(state): State<TenancyState>,
    member: TeamMember,
    context: audit::Context,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    member.access.require_administrator("rename this team")?;
    let name = validate_name(&body.name, "team")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let team: Team = connection
        .transaction::<Team, diesel::result::Error, _>(async |transaction| {
            let team: Team = diesel::update(teams::table.find(member.team.id))
                .set(teams::name.eq(&name))
                .returning(Team::as_returning())
                .get_result(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::TEAM_RENAMED, "Team")
                    .team(team.id)
                    .subject(team.id)
                    .label(&team.name)
                    .changes(Changes::new().field(
                        "name",
                        member.team.name.clone(),
                        team.name.clone(),
                    )),
            )
            .await?;

            Ok(team)
        })
        .await
        .map_err(log_internal)?;

    Ok(Json(TeamBody { team }))
}

/// Replaces a member's roles in the team, or on one record of it.
///
/// Roles are replaced wholesale rather than patched, so the request states the
/// end state and two managers editing the same member cannot interleave into
/// a set neither asked for. With a `resource_id`, the grants replaced are the
/// ones narrowed to that record, and both the caller's standing and the
/// member's are measured there: somebody granted a role on one record manages
/// that record, and nothing wider.
async fn change_member_roles(
    State(state): State<TenancyState>,
    member: TeamMember,
    context: audit::Context,
    Path((_team_id, membership_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RolesBody>,
) -> Result<impl IntoResponse, ApiError> {
    let team = member.team.clone();
    let resources: Vec<Uuid> = body.resource_id.into_iter().collect();

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let held = connection
        .transaction::<Vec<Role>, ApiError, _>(async |transaction| {
            lock_team(transaction, &team).await?;
            let actor =
                team_standing(transaction, &state.roles, &member.user, &team, &resources).await?;
            actor.require(Action::Update, MEMBERSHIP)?;

            let target = find_member(transaction, team.id, membership_id).await?;
            let target_access =
                team_membership_access(transaction, &state.roles, &team, target.id, &resources)
                    .await?;
            hierarchy::require_outranks_member(
                &actor,
                &target_access,
                target.user_id == Some(member.user.id),
            )?;

            let holder = Holder::Team {
                team_id: team.id,
                membership_id: target.id,
                resource_id: body.resource_id,
            };
            let change = replace_grants(
                transaction,
                &state.roles,
                &actor,
                team.organization_id,
                holder,
                Scope::Team,
                &body.role_ids,
                member.user.id,
            )
            .await?;

            // Only a team-wide administrator counts toward the invariant, and
            // only a claimed one: an invitation is not a person.
            if change.revoked_administrator
                && body.resource_id.is_none()
                && target.user_id.is_some()
            {
                require_team_keeps_an_admin(transaction, team.id, "step down").await?;
            }

            if change.is_noop() {
                return Ok(change.after);
            }

            // A grant narrowed to one record says which, so the log never
            // reads as a team-wide change that did not happen.
            let label = member_label(transaction, target.id).await?;
            let label = match body.resource_id {
                Some(resource_id) => format!("{label} (record {resource_id})"),
                None => label,
            };
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_ROLE_CHANGED, "TeamMembership")
                    .team(team.id)
                    .subject(target.id)
                    .label(&label)
                    .changes(change.changes(&state.roles)),
            )
            .await?;

            // Only a claimed membership has somebody to tell, and a manager
            // editing their own roles already knows. The notice commits with
            // the change, so a request that ends in a `409` sends nothing.
            if let Some(target_user_id) = target.user_id
                && target_user_id != member.user.id
            {
                notify_roles_changed(
                    transaction,
                    &state.roles,
                    target_user_id,
                    Some(team.id),
                    &team.name,
                    &change.after,
                    &crate::notifications::team_settings_href(team.id),
                )
                .await?;
            }

            Ok(change.after)
        })
        .await?;

    Ok(Json(MembershipBody {
        membership_id,
        roles: held.iter().map(|role| role.summary(&state.roles)).collect(),
    }))
}

/// Removes someone else from the team.
///
/// Removing a pending member cancels their invitation with it, since the
/// invitation's row cascades from the membership it pre-created, and every
/// grant the membership held goes with it.
async fn remove_member(
    State(state): State<TenancyState>,
    member: TeamMember,
    context: audit::Context,
    Path((_team_id, membership_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let team = member.team.clone();

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_team(transaction, &team).await?;
            let actor = team_standing(transaction, &state.roles, &member.user, &team, &[]).await?;
            actor.require(Action::Destroy, MEMBERSHIP)?;

            let target = find_member(transaction, team.id, membership_id).await?;
            if target.user_id == Some(member.user.id) {
                return Err(ApiError::validation(
                    "Use the leave endpoint to leave a team yourself.",
                ));
            }
            let target_access =
                team_membership_access(transaction, &state.roles, &team, target.id, &[]).await?;
            hierarchy::require_outranks_member(&actor, &target_access, false)?;

            // Read before the delete: afterwards the membership, and the
            // invitation that may be all this person ever had, are both gone.
            let label = member_label(transaction, target.id).await?;

            diesel::delete(team_memberships::table.find(target.id))
                .execute(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_REMOVED, "TeamMembership")
                    .team(team.id)
                    .subject(target.id)
                    .label(&label)
                    .changes(Changes::new().field(
                        "roles",
                        held_names(&target_access),
                        Vec::<String>::new(),
                    )),
            )
            .await?;

            // The caller outranks the target, so this only fires when the
            // caller's own administrator role was taken by a request that
            // committed while this one was in flight.
            if target.user_id.is_some() && target_access.is_administrator() {
                require_team_keeps_an_admin(transaction, team.id, "remove them").await?;
            }
            state
                .queue_seat_sync(transaction, team.organization_id)
                .await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Leaves the team, provided the team keeps an administrator.
async fn leave_team(
    State(state): State<TenancyState>,
    member: TeamMember,
    context: audit::Context,
) -> Result<impl IntoResponse, ApiError> {
    let team = member.team.clone();

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_team(transaction, &team).await?;
            // Re-read under the lock: a request that committed while this one
            // was in flight may already have taken the caller's administrator
            // role, in which case they are free to go.
            let leaving = find_member(transaction, team.id, member.membership.id).await?;
            let leaving_access =
                team_membership_access(transaction, &state.roles, &team, leaving.id, &[]).await?;

            diesel::delete(team_memberships::table.find(leaving.id))
                .execute(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::MEMBER_LEFT, "TeamMembership")
                    .team(team.id)
                    .subject(leaving.id)
                    .label(&audit::person_label(
                        member.user.first_name.as_deref(),
                        member.user.last_name.as_deref(),
                        &member.user.email,
                    )),
            )
            .await?;

            if leaving_access.is_administrator() {
                require_team_keeps_an_admin(transaction, team.id, "leave").await?;
            }
            state
                .queue_seat_sync(transaction, team.organization_id)
                .await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Revokes a pending team invitation.
///
/// An invitation is a member who has not arrived yet, holding what it offers,
/// so taking it back is held to the rule removing a member is: nobody revokes
/// an invitation offering a role at or above their own.
async fn revoke_team_invitation(
    State(state): State<TenancyState>,
    member: TeamMember,
    context: audit::Context,
    Path((_team_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let team = member.team.clone();

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_team(transaction, &team).await?;
            let actor = team_standing(transaction, &state.roles, &member.user, &team, &[]).await?;
            actor.require(Action::Destroy, MEMBERSHIP)?;

            let pending: (Option<Uuid>, String) = invitations::table
                .filter(invitations::id.eq(invitation_id))
                .filter(invitations::team_id.eq(team.id))
                .select((invitations::team_membership_id, invitations::email))
                .first(transaction)
                .await
                .optional()?
                .ok_or_else(ApiError::not_found)?;
            let offered =
                offered_by_invitation(transaction, team.organization_id, invitation_id, pending.0)
                    .await?;
            hierarchy::require_outranks_offer(&actor, &offered)?;

            revoke_invitation(transaction, invitation_id, pending.0).await?;
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::INVITATION_REVOKED, "Invitation")
                    .team(team.id)
                    .subject(invitation_id)
                    .label(&pending.1),
            )
            .await?;

            // An invitation holds a seat until it is claimed or taken back.
            state
                .queue_seat_sync(transaction, team.organization_id)
                .await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Revokes any pending invitation in the organization, team ones included.
///
/// The organization's managers may invite into any team of the organization,
/// so they may take those invitations back, under the same hierarchy as a
/// team's own managers.
async fn revoke_organization_invitation(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Destroy, MEMBERSHIP)?;

            let pending: (Option<Uuid>, String, Option<Uuid>) = invitations::table
                .filter(invitations::id.eq(invitation_id))
                .filter(invitations::organization_id.eq(organization_id))
                .select((
                    invitations::team_membership_id,
                    invitations::email,
                    invitations::team_id,
                ))
                .first(transaction)
                .await
                .optional()?
                .ok_or_else(ApiError::not_found)?;
            let (pending_membership, email, team_id) = pending;

            let offered = offered_by_invitation(
                transaction,
                organization_id,
                invitation_id,
                pending_membership,
            )
            .await?;
            hierarchy::require_outranks_offer(&actor, &offered)?;

            revoke_invitation(transaction, invitation_id, pending_membership).await?;

            // An organization manager may revoke an invitation into one of its
            // teams, and that belongs in the team's log rather than the
            // organization's.
            let event = audit::Event::new(audit::INVITATION_REVOKED, "Invitation")
                .subject(invitation_id)
                .label(&email);
            let event = match team_id {
                Some(team_id) => event.team(team_id),
                None => event.organization(organization_id),
            };
            audit::record(transaction, &context.by(&member.user), &event).await?;

            state.queue_seat_sync(transaction, organization_id).await?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The roles a pending invitation offers, loaded.
async fn offered_by_invitation(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    invitation_id: Uuid,
    pending_membership: Option<Uuid>,
) -> Result<Vec<Role>, ApiError> {
    let role_ids = invitation::offered_by(connection, invitation_id, pending_membership).await?;
    load_roles(connection, organization_id, &role_ids).await
}

/// Deletes a pending invitation and the unclaimed membership it pre-created.
///
/// Organization invitations create no membership and pass `None`. A claimed
/// invitation no longer exists at all, so claiming and revoking race to the
/// same honest answer: `404` for whoever arrives second.
async fn revoke_invitation(
    connection: &mut AsyncPgConnection,
    invitation_id: Uuid,
    pending_membership: Option<Uuid>,
) -> Result<(), ApiError> {
    diesel::delete(invitations::table.find(invitation_id))
        .execute(connection)
        .await?;

    if let Some(membership_id) = pending_membership {
        diesel::delete(
            team_memberships::table
                .filter(team_memberships::id.eq(membership_id))
                .filter(team_memberships::user_id.is_null()),
        )
        .execute(connection)
        .await?;
    }

    Ok(())
}

/// What a replacement of one holder's grants did.
struct GrantChange {
    /// The roles held at the scope before, highest first.
    before: Vec<Role>,
    /// The roles held at the scope after, highest first.
    after: Vec<Role>,
    /// Whether the administrator role was among those taken away.
    revoked_administrator: bool,
}

impl GrantChange {
    fn is_noop(&self) -> bool {
        let before: BTreeSet<Uuid> = self.before.iter().map(|role| role.id).collect();
        let after: BTreeSet<Uuid> = self.after.iter().map(|role| role.id).collect();
        before == after
    }

    fn changes(&self, roles: &RoleSet) -> Changes {
        Changes::new().field(
            "roles",
            role_names(&self.before, roles),
            role_names(&self.after, roles),
        )
    }
}

/// Replaces the roles `holder` is granted at exactly its scope.
///
/// Every role the request adds is held to [`hierarchy::require_assignable`],
/// and every role it takes away to [`hierarchy::require_outranks_role`], both
/// against the caller's standing at that scope. The default role is never
/// granted, so asking for it is answered the way granting it would be.
#[expect(
    clippy::too_many_arguments,
    reason = "one call site per scope, each naming the scope it replaces at"
)]
async fn replace_grants(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    actor: &Access,
    organization_id: Uuid,
    holder: Holder,
    tier: Scope,
    requested_ids: &[Uuid],
    granted_by: Uuid,
) -> Result<GrantChange, ApiError> {
    let current_ids = grants::held_role_ids(connection, holder).await?;
    let before = load_roles(connection, organization_id, &current_ids).await?;
    let after = load_roles(connection, organization_id, requested_ids).await?;

    let mut revoked_administrator = false;
    for role in &before {
        if after.iter().any(|kept| kept.id == role.id) {
            continue;
        }
        hierarchy::require_outranks_role(actor, role)?;
        revoked_administrator |= role.is_administrator();
        grants::revoke(connection, role.id, holder).await?;
    }
    for role in &after {
        if before.iter().any(|held| held.id == role.id) {
            continue;
        }
        hierarchy::require_assignable(actor, role, tier, roles)?;
        grants::grant(
            connection,
            organization_id,
            role.id,
            holder,
            Some(granted_by),
        )
        .await?;
    }

    Ok(GrantChange {
        before,
        after,
        revoked_administrator,
    })
}

/// Tells a member their roles changed, naming the ones they now hold.
async fn notify_roles_changed(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    team_id: Option<Uuid>,
    place: &str,
    held: &[Role],
    href: &str,
) -> Result<(), ApiError> {
    let body = if held.is_empty() {
        "You now hold only the default role.".to_owned()
    } else {
        format!("You now hold: {}.", role_names(held, roles).join(", "))
    };
    crate::notifications::notify(
        connection,
        crate::notifications::NewNotification {
            user_id,
            team_id,
            kind: crate::notifications::MEMBERSHIP_ROLES_CHANGED,
            title: &format!("Your role in {place} changed"),
            body: Some(&body),
            href: Some(href),
        },
    )
    .await?;
    Ok(())
}

/// The names of the roles an access holds beyond the default role.
fn held_names(access: &Access) -> Vec<String> {
    access
        .roles()
        .iter()
        .filter(|role| role.position > 0)
        .map(|role| role.name.clone())
        .collect()
}

/// The caller's organization-wide standing, read again under the lock.
///
/// The guard's snapshot was taken before the lock, and a request that
/// committed since may have changed it; acting on the stale one is how a
/// demoted manager would get one last change through.
pub(super) async fn organization_standing(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user: &User,
    organization_id: Uuid,
) -> Result<Access, ApiError> {
    resolve_organization_access(connection, roles, user.id, organization_id)
        .await?
        .map(|(_organization, _membership, access)| access)
        .ok_or_else(ApiError::not_found)
}

/// The caller's standing in a team, read again under the lock.
async fn team_standing(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user: &User,
    team: &Team,
    resources: &[Uuid],
) -> Result<Access, ApiError> {
    resolve_team_access(connection, roles, user.id, team.id, resources)
        .await?
        .map(|(_team, _membership, access)| access)
        .ok_or_else(ApiError::not_found)
}

/// How a team membership reads in the audit log.
///
/// Delegates to the roster's own labeller, so the name an admin saw in the
/// roster is the name they see in the log, and a pending member reads as the
/// address their invitation went to rather than as a blank.
///
/// A membership that has just vanished (a race with another admin) has no
/// label; the event still records what happened, and the id it names is the
/// answer to what it happened to.
async fn member_label(
    connection: &mut AsyncPgConnection,
    membership_id: Uuid,
) -> Result<String, ApiError> {
    let labels = TeamMembership::labels_for(connection, &[membership_id]).await?;
    Ok(labels
        .get(&membership_id)
        .cloned()
        .unwrap_or_else(|| membership_id.to_string()))
}

/// How an account reads in the audit log, for a surface holding only their id.
///
/// Selects the three columns the label is built from rather than loading a
/// whole account, which is all a label ever needs.
async fn account_label(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<String, ApiError> {
    let account: (Option<String>, Option<String>, String) = users::table
        .find(user_id)
        .select((users::first_name, users::last_name, users::email))
        .first(connection)
        .await?;

    Ok(audit::person_label(
        account.0.as_deref(),
        account.1.as_deref(),
        &account.2,
    ))
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

/// Locks a team for the length of the caller's transaction, organization first.
///
/// The last-administrator rule is a read of who else administers the team
/// followed by a write to one membership. Left unordered, two administrators
/// acting at the same instant each read the other, each commit, and the team
/// is left without one: the `409` never fires because neither request ever saw
/// the other coming.
///
/// Every role and membership change in an organization takes the
/// organization's row first, so a team change and an organization change (a
/// role deleted while somebody is granted it, say) are ordered too. One order,
/// organization then team, everywhere, because two transactions each locking
/// the other's next row deadlock, and a deadlock is a `500` where a `409`
/// belongs.
///
/// Answers `404` when the team is gone, which a concurrent deletion can do
/// between the guard and here.
async fn lock_team(connection: &mut AsyncPgConnection, team: &Team) -> Result<(), ApiError> {
    lock_organization(connection, team.organization_id).await?;
    teams::table
        .find(team.id)
        .select(teams::id)
        .for_update()
        .first::<Uuid>(connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)
        .map(|_id| ())
}

/// Locks an organization for the length of the caller's transaction.
///
/// The first lock of every role, grant, membership, and invitation change in
/// the organization; see [`lock_team`] for why there is one order. Invitation
/// creation takes it too, so the seats limit is counted under the same order
/// the membership rules are.
pub(super) async fn lock_organization(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<(), ApiError> {
    organizations::table
        .find(organization_id)
        .select(organizations::id)
        .for_update()
        .first::<Uuid>(connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)
        .map(|_id| ())
}

/// Refuses a change that left the team with no claimed administrator.
///
/// Counted after the change rather than before it, so one rule covers
/// demotion, removal, and leaving alike, and the transaction rolls the change
/// back when it fires. Unclaimed memberships never count, because an
/// invitation is not a person.
async fn require_team_keeps_an_admin(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    attempt: &str,
) -> Result<(), ApiError> {
    if grants::team_administrators(connection, team_id)
        .await?
        .is_empty()
    {
        Err(last_admin_conflict("team", attempt))
    } else {
        Ok(())
    }
}

/// Refuses a change that left the organization with no administrator.
///
/// The organization half of [`require_team_keeps_an_admin`]. Organization
/// memberships exist only once claimed, so every one of them counts.
async fn require_organization_keeps_an_admin(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    attempt: &str,
) -> Result<(), ApiError> {
    if grants::organization_administrators(connection, organization_id)
        .await?
        .is_empty()
    {
        Err(last_admin_conflict("organization", attempt))
    } else {
        Ok(())
    }
}

/// Trims and bounds a submitted display name.
///
/// Control characters are refused along with the wrong length. A tenant's name
/// is rendered into an invitation's subject line and into the UI, and a line
/// break in either is at best a display bug and at worst an attempt at a
/// header of the caller's own. The mail layer encodes what it is given (see
/// `crate::mail`), so this is the second lock rather than the only one.
fn validate_name(raw: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::validation(format!(
            "Give the {label} a name of 1 to {MAX_NAME_CHARS} characters."
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ApiError::validation(format!(
            "Give the {label} a name without line breaks or control characters."
        )));
    }
    Ok(trimmed.to_owned())
}

/// The refusal that keeps every tenant administrable.
///
/// Normally only the last administrator acting on themselves reaches it: an
/// administrator editing somebody else still counts as the one the tenant is
/// left with. The exception is a race, where the caller's own administrator
/// role was taken by a request that committed while theirs was in flight; the
/// organization lock orders the two so exactly one of them gets this answer.
fn last_admin_conflict(tenant: &str, attempt: &str) -> ApiError {
    ApiError::conflict(format!(
        "A {tenant} needs at least one administrator. \
         Give someone else the administrator role before you {attempt}."
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

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(
            validate_name("  Ops  ", "team").as_deref().ok(),
            Some("Ops")
        );
        validate_name("   ", "team").expect_err("a blank name is refused");
        validate_name(&"x".repeat(101), "team").expect_err("a name is bounded");
        validate_name("line\nbreak", "team").expect_err("a name is one line");
    }
}
