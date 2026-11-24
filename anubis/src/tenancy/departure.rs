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
//! 2. **Surviving tenants keep an administrator.** An organization or team
//!    that still has members but lost its last administrator grants the
//!    administrator role to its longest-standing remaining member. The
//!    invariant the interactive endpoints defend by refusing, this one defends
//!    by succeeding.
//!
//! A surviving team with no members left is kept: it holds application records
//! that chain to it, and an organization admin can delete it or invite into it.

use std::collections::BTreeSet;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::{
    organization_memberships, organizations, sub_tenant_memberships, sub_tenants, team_memberships,
    teams,
};
use crate::tenancy::grants::{self, Holder};
use crate::tenancy::model::{NewOrganizationMembership, OrganizationAccess};
use crate::tenancy::role;

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

    // An organization is affected when the user held a membership in it, in
    // one of its sub-tenants, or in one of its teams; a member at any of those
    // levels still keeps it alive.
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
    let sub_tenant_organization_ids: Vec<Uuid> = sub_tenant_memberships::table
        .inner_join(sub_tenants::table)
        .filter(sub_tenant_memberships::user_id.eq(user_id))
        .select(sub_tenants::organization_id)
        .load(connection)
        .await?;
    organization_ids.extend(sub_tenant_organization_ids);

    // Every role and grant change locks its organization first, so the
    // settlement does too, and in one order (Postgres locks the rows as the
    // sort returns them) so two departures never deadlock. The administrator
    // promoted below is then chosen from the roster as it stands, never from
    // one a concurrent grant or revocation is still rewriting.
    organizations::table
        .filter(organizations::id.eq_any(&organization_ids))
        .order(organizations::id)
        .select(organizations::id)
        .for_update()
        .load::<Uuid>(connection)
        .await?;

    diesel::delete(
        organization_memberships::table.filter(organization_memberships::user_id.eq(user_id)),
    )
    .execute(connection)
    .await?;
    diesel::delete(
        sub_tenant_memberships::table.filter(sub_tenant_memberships::user_id.eq(user_id)),
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
    if team_members > 0 {
        return Ok(false);
    }

    // A guest granted one sub-tenant and nothing else is still somebody who
    // can reach the organization's work, so their grant keeps it alive.
    let sub_tenant_members: i64 = sub_tenant_memberships::table
        .inner_join(sub_tenants::table)
        .filter(sub_tenants::organization_id.eq(organization_id))
        .count()
        .get_result(connection)
        .await?;

    Ok(sub_tenant_members == 0)
}

/// Grants the administrator role to the longest-standing member when an
/// organization has none left.
///
/// Who that is, [`organization_successor`] decides. A successor reached only
/// through a team or a sub-tenant gains the organization membership too.
async fn ensure_organization_admin(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<(), diesel::result::Error> {
    if !grants::organization_administrators(connection, organization_id)
        .await?
        .is_empty()
    {
        return Ok(());
    }

    let Some(successor) = organization_successor(connection, organization_id).await? else {
        tracing::warn!(
            organization.id = %organization_id,
            "organization {{organization.id}} survives with nobody who could administer it",
        );
        return Ok(());
    };

    let membership_id = match successor {
        Successor::Member(membership_id) => membership_id,
        Successor::Newcomer(user_id) => {
            diesel::insert_into(organization_memberships::table)
                .values(NewOrganizationMembership {
                    organization_id,
                    user_id,
                })
                .returning(organization_memberships::id)
                .get_result(connection)
                .await?
        }
    };
    let administrator = role::administrator_role(connection, organization_id).await?;
    grants::grant(
        connection,
        organization_id,
        administrator.id,
        Holder::Organization { membership_id },
        None,
    )
    .await
}

/// Who takes the administrator role over when an organization has none left.
enum Successor {
    /// An organization member, by membership.
    Member(Uuid),
    /// Somebody the organization reaches only through a team or a sub-tenant,
    /// by account, who gains an organization membership with the role.
    Newcomer(Uuid),
}

/// The longest-standing member able to use the administrator role.
///
/// In order: an organization member who is not suspended, full members before
/// guests, because the role carries a guest past the guest boundary and an
/// organization left with only guests is better administered by one of them
/// than by nobody. Then, when the organization survives through its teams or
/// its sub-tenants, a member of either who holds no organization membership,
/// teams first because a team member works in the organization where a
/// sub-tenant grant may be a guest's, and never one suspended in the
/// sub-tenant they are reached through. Last, when everybody left is
/// suspended, the longest-standing of them, full members first: the role
/// gives nothing while the suspension holds, and without it nobody could ever
/// grant the role again once the application lifts one.
async fn organization_successor(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<Option<Successor>, diesel::result::Error> {
    let members_first = (
        organization_memberships::access
            .eq(OrganizationAccess::Guest)
            .asc(),
        organization_memberships::created_at.asc(),
    );

    let reachable: Option<Uuid> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::suspended_at.is_null())
        .select(organization_memberships::id)
        .order(members_first)
        .first(connection)
        .await
        .optional()?;
    if let Some(membership_id) = reachable {
        return Ok(Some(Successor::Member(membership_id)));
    }

    let through_team: Option<Uuid> = team_memberships::table
        .inner_join(teams::table)
        .left_join(
            organization_memberships::table.on(organization_memberships::organization_id
                .eq(teams::organization_id)
                .and(
                    organization_memberships::user_id
                        .nullable()
                        .eq(team_memberships::user_id),
                )),
        )
        .left_join(
            sub_tenant_memberships::table.on(sub_tenant_memberships::sub_tenant_id
                .nullable()
                .eq(teams::sub_tenant_id)
                .and(
                    sub_tenant_memberships::user_id
                        .nullable()
                        .eq(team_memberships::user_id),
                )),
        )
        .filter(teams::organization_id.eq(organization_id))
        .filter(team_memberships::user_id.is_not_null())
        .filter(organization_memberships::id.is_null())
        .filter(sub_tenant_memberships::suspended_at.is_null())
        .select(team_memberships::user_id)
        .order(team_memberships::created_at.asc())
        .first(connection)
        .await
        .optional()?
        .flatten();
    if let Some(user_id) = through_team {
        return Ok(Some(Successor::Newcomer(user_id)));
    }

    let through_sub_tenant: Option<Uuid> = sub_tenant_memberships::table
        .inner_join(sub_tenants::table)
        .left_join(
            organization_memberships::table.on(organization_memberships::organization_id
                .eq(sub_tenants::organization_id)
                .and(organization_memberships::user_id.eq(sub_tenant_memberships::user_id))),
        )
        .filter(sub_tenants::organization_id.eq(organization_id))
        .filter(sub_tenant_memberships::suspended_at.is_null())
        .filter(organization_memberships::id.is_null())
        .select(sub_tenant_memberships::user_id)
        .order(sub_tenant_memberships::created_at.asc())
        .first(connection)
        .await
        .optional()?;
    if let Some(user_id) = through_sub_tenant {
        return Ok(Some(Successor::Newcomer(user_id)));
    }

    let suspended: Option<Uuid> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .select(organization_memberships::id)
        .order(members_first)
        .first(connection)
        .await
        .optional()?;
    Ok(suspended.map(Successor::Member))
}

/// Grants the administrator role to the longest-standing claimed member when a
/// team has none left.
async fn ensure_team_admin(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
) -> Result<(), diesel::result::Error> {
    // A team of an organization deleted above is gone, and settles to nothing.
    let organization_id: Option<Uuid> = teams::table
        .find(team_id)
        .select(teams::organization_id)
        .first(connection)
        .await
        .optional()?;
    let Some(organization_id) = organization_id else {
        return Ok(());
    };

    if !grants::team_administrators(connection, team_id)
        .await?
        .is_empty()
    {
        return Ok(());
    }

    // An empty team keeps its records and waits for an organization admin. A
    // suspended member, in the organization or in the sub-tenant the team is
    // scoped to, reaches nothing, so the role never goes to them.
    let longest_standing: Option<Uuid> = team_memberships::table
        .inner_join(teams::table)
        .left_join(
            organization_memberships::table.on(organization_memberships::organization_id
                .eq(teams::organization_id)
                .and(
                    organization_memberships::user_id
                        .nullable()
                        .eq(team_memberships::user_id),
                )),
        )
        .left_join(
            sub_tenant_memberships::table.on(sub_tenant_memberships::sub_tenant_id
                .nullable()
                .eq(teams::sub_tenant_id)
                .and(
                    sub_tenant_memberships::user_id
                        .nullable()
                        .eq(team_memberships::user_id),
                )),
        )
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_not_null())
        .filter(organization_memberships::suspended_at.is_null())
        .filter(sub_tenant_memberships::suspended_at.is_null())
        .select(team_memberships::id)
        .order(team_memberships::created_at.asc())
        .first(connection)
        .await
        .optional()?;
    // Everybody left suspended still leaves the role with the longest-standing
    // of them, for the reason the organization does.
    let longest_standing = match longest_standing {
        Some(membership_id) => Some(membership_id),
        None => team_memberships::table
            .filter(team_memberships::team_id.eq(team_id))
            .filter(team_memberships::user_id.is_not_null())
            .select(team_memberships::id)
            .order(team_memberships::created_at.asc())
            .first(connection)
            .await
            .optional()?,
    };
    let Some(membership_id) = longest_standing else {
        return Ok(());
    };

    let administrator = role::administrator_role(connection, organization_id).await?;
    grants::grant(
        connection,
        organization_id,
        administrator.id,
        Holder::Team {
            team_id,
            membership_id,
            resource_id: None,
        },
        None,
    )
    .await
}
