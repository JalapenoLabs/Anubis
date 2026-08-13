//! What becomes of organizations and teams when a user leaves for good.
//!
//! Account deletion is terminal and must always succeed, so it cannot refuse
//! the way [`leave a team`](super::routes) does. Instead it settles what the
//! departing user leaves behind, under two rules:
//!
//! 1. **Deserted tenants are deleted.** An organization nobody can still reach,
//!    with no organization memberships and no claimed team memberships in any
//!    of its teams, is deleted along with everything that chains to it. This is
//!    what keeps a personal organization from outliving its only member.
//! 2. **Surviving tenants keep an admin.** An organization or team that still
//!    has members but lost its last admin promotes its longest-standing
//!    remaining member. The invariant the interactive endpoints defend by
//!    refusing, this one defends by succeeding.
//!
//! A surviving team with no members left is kept: it holds application records
//! that chain to it, and an organization admin can delete it or invite into it.

use std::collections::BTreeSet;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::{organization_memberships, organizations, team_memberships, teams};
use crate::tenancy::bootstrap::{ADMIN_ROLE, holds_admin};
use crate::tenancy::model::NewOrganizationMembership;

/// Removes a user's memberships and settles the tenants they leave behind.
///
/// Run inside the transaction that deletes the user, before the user row goes:
/// the memberships are removed here rather than left to the foreign key
/// cascade, so the settlement below sees the world as it will be.
pub(crate) async fn settle_departure(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), diesel::result::Error> {
    let team_ids: Vec<Uuid> = team_memberships::table
        .filter(team_memberships::user_id.eq(user_id))
        .select(team_memberships::team_id)
        .load(connection)
        .await?;

    // An organization is affected when the user held a membership in it, or in
    // one of its teams; a team-only member still keeps it alive.
    let mut organization_ids: BTreeSet<Uuid> = organization_memberships::table
        .filter(organization_memberships::user_id.eq(user_id))
        .select(organization_memberships::organization_id)
        .load(connection)
        .await?
        .into_iter()
        .collect();
    let team_organization_ids: Vec<Uuid> = teams::table
        .filter(teams::id.eq_any(&team_ids))
        .select(teams::organization_id)
        .load(connection)
        .await?;
    organization_ids.extend(team_organization_ids);

    diesel::delete(
        organization_memberships::table.filter(organization_memberships::user_id.eq(user_id)),
    )
    .execute(connection)
    .await?;
    diesel::delete(team_memberships::table.filter(team_memberships::user_id.eq(user_id)))
        .execute(connection)
        .await?;

    for organization_id in organization_ids {
        if is_deserted(connection, organization_id).await? {
            diesel::delete(organizations::table.find(organization_id))
                .execute(connection)
                .await?;
        } else {
            ensure_organization_admin(connection, organization_id).await?;
        }
    }

    // Teams of a deleted organization are gone with it, and settle to no-ops.
    for team_id in team_ids {
        ensure_team_admin(connection, team_id).await?;
    }

    Ok(())
}

/// Returns `true` when nobody can reach the organization any more.
async fn is_deserted(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let organization_members: i64 = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .count()
        .get_result(connection)
        .await?;
    if organization_members > 0 {
        return Ok(false);
    }

    // Unclaimed memberships belong to invitations, not to people who are here.
    let team_members: i64 = team_memberships::table
        .inner_join(teams::table)
        .filter(teams::organization_id.eq(organization_id))
        .filter(team_memberships::user_id.is_not_null())
        .count()
        .get_result(connection)
        .await?;

    Ok(team_members == 0)
}

/// Promotes the longest-standing member when an organization has no admin.
async fn ensure_organization_admin(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<(), diesel::result::Error> {
    let memberships: Vec<(Uuid, Vec<String>)> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .select((
            organization_memberships::id,
            organization_memberships::roles,
        ))
        .order(organization_memberships::created_at.asc())
        .load(connection)
        .await?;

    if memberships.iter().any(|(_id, roles)| holds_admin(roles)) {
        return Ok(());
    }

    if let Some((membership_id, mut roles)) = memberships.into_iter().next() {
        roles.push(ADMIN_ROLE.to_owned());
        diesel::update(organization_memberships::table.find(membership_id))
            .set(organization_memberships::roles.eq(roles))
            .execute(connection)
            .await?;
        return Ok(());
    }

    // The organization has no organization memberships at all, and survives
    // only through its teams: the longest-standing team member takes it over.
    let successor: Option<Uuid> = team_memberships::table
        .inner_join(teams::table)
        .filter(teams::organization_id.eq(organization_id))
        .filter(team_memberships::user_id.is_not_null())
        .select(team_memberships::user_id)
        .order(team_memberships::created_at.asc())
        .first(connection)
        .await
        .optional()?
        .flatten();

    if let Some(successor) = successor {
        diesel::insert_into(organization_memberships::table)
            .values(NewOrganizationMembership {
                organization_id,
                user_id: successor,
                roles: &[ADMIN_ROLE.to_owned()],
            })
            .execute(connection)
            .await?;
    }

    Ok(())
}

/// Promotes the longest-standing claimed member when a team has no admin.
async fn ensure_team_admin(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
) -> Result<(), diesel::result::Error> {
    let memberships: Vec<(Uuid, Vec<String>)> = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_not_null())
        .select((team_memberships::id, team_memberships::roles))
        .order(team_memberships::created_at.asc())
        .load(connection)
        .await?;

    if memberships.iter().any(|(_id, roles)| holds_admin(roles)) {
        return Ok(());
    }

    // An empty team keeps its records and waits for an organization admin.
    let Some((membership_id, mut roles)) = memberships.into_iter().next() else {
        return Ok(());
    };

    roles.push(ADMIN_ROLE.to_owned());
    diesel::update(team_memberships::table.find(membership_id))
        .set(team_memberships::roles.eq(roles))
        .execute(connection)
        .await?;

    Ok(())
}
