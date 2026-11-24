//! Role grants: one role held through one membership.
//!
//! The membership a grant hangs off is its scope. An organization membership
//! grants organization-wide, a sub-tenant membership in that sub-tenant, a team
//! membership in that team, and a team membership with a record in that one
//! record and everything nested under it. Removing a membership removes its
//! grants in the same statement (a cascading foreign key), which is what keeps
//! somebody removed and invited back from finding their old roles waiting.

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::roles::ADMIN_ROLE;
use crate::schema::{organization_memberships, role_grants, roles as role_rows, team_memberships};

/// The membership a grant hangs off, which is the grant's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holder {
    /// Organization-wide, through an organization membership.
    Organization {
        /// The organization membership.
        membership_id: Uuid,
    },
    /// In one team, or in one record of it, through a team membership.
    Team {
        /// The team.
        team_id: Uuid,
        /// The team membership, claimed or still an invitation.
        membership_id: Uuid,
        /// The record the grant is narrowed to, or `None` for the whole team.
        resource_id: Option<Uuid>,
    },
}

#[derive(Insertable)]
#[diesel(table_name = role_grants)]
struct NewRoleGrant {
    organization_id: Uuid,
    role_id: Uuid,
    organization_membership_id: Option<Uuid>,
    team_id: Option<Uuid>,
    team_membership_id: Option<Uuid>,
    resource_id: Option<Uuid>,
    granted_by: Option<Uuid>,
}

/// Grants `role_id` through `holder`; granting a role already held changes
/// nothing.
///
/// The composite foreign keys refuse a role, a membership, or a team of
/// another organization, so a caller that skipped its own check still cannot
/// write a grant across organizations.
///
/// # Errors
/// Returns the underlying Diesel error when the insert fails.
pub(crate) async fn grant(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    role_id: Uuid,
    holder: Holder,
    granted_by: Option<Uuid>,
) -> QueryResult<()> {
    let (organization_membership_id, team_id, team_membership_id, resource_id) = match holder {
        Holder::Organization { membership_id } => (Some(membership_id), None, None, None),
        Holder::Team {
            team_id,
            membership_id,
            resource_id,
        } => (None, Some(team_id), Some(membership_id), resource_id),
    };

    diesel::insert_into(role_grants::table)
        .values(NewRoleGrant {
            organization_id,
            role_id,
            organization_membership_id,
            team_id,
            team_membership_id,
            resource_id,
            granted_by,
        })
        .on_conflict_do_nothing()
        .execute(connection)
        .await?;
    Ok(())
}

/// Revokes `role_id` from `holder`, at exactly that scope.
///
/// # Errors
/// Returns the underlying Diesel error when the delete fails.
pub(crate) async fn revoke(
    connection: &mut AsyncPgConnection,
    role_id: Uuid,
    holder: Holder,
) -> QueryResult<()> {
    match holder {
        Holder::Organization { membership_id } => {
            diesel::delete(
                role_grants::table
                    .filter(role_grants::role_id.eq(role_id))
                    .filter(role_grants::organization_membership_id.eq(membership_id)),
            )
            .execute(connection)
            .await?;
        }
        Holder::Team {
            membership_id,
            resource_id,
            ..
        } => {
            diesel::delete(
                role_grants::table
                    .filter(role_grants::role_id.eq(role_id))
                    .filter(role_grants::team_membership_id.eq(membership_id))
                    .filter(role_grants::resource_id.is_not_distinct_from(resource_id)),
            )
            .execute(connection)
            .await?;
        }
    }
    Ok(())
}

/// The roles `holder` is granted at exactly its scope, nothing wider or narrower.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn held_role_ids(
    connection: &mut AsyncPgConnection,
    holder: Holder,
) -> QueryResult<Vec<Uuid>> {
    match holder {
        Holder::Organization { membership_id } => {
            role_grants::table
                .filter(role_grants::organization_membership_id.eq(membership_id))
                .select(role_grants::role_id)
                .load(connection)
                .await
        }
        Holder::Team {
            membership_id,
            resource_id,
            ..
        } => {
            role_grants::table
                .filter(role_grants::team_membership_id.eq(membership_id))
                .filter(role_grants::resource_id.is_not_distinct_from(resource_id))
                .select(role_grants::role_id)
                .load(connection)
                .await
        }
    }
}

/// The people holding the administrator role team-wide, by account.
///
/// Claimed memberships only, because an invitation is not a person: it cannot
/// administer anything and it cannot read an inbox. Ordered by account so a
/// caller notifying several people writes their rows in a stable order.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn team_administrators(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    let holders: Vec<Option<Uuid>> = role_grants::table
        .inner_join(role_rows::table)
        .inner_join(
            team_memberships::table.on(team_memberships::id
                .nullable()
                .eq(role_grants::team_membership_id)),
        )
        .filter(role_grants::team_id.eq(team_id))
        .filter(role_grants::resource_id.is_null())
        .filter(role_rows::key.eq(ADMIN_ROLE))
        .filter(team_memberships::user_id.is_not_null())
        .select(team_memberships::user_id)
        .order(team_memberships::user_id.asc())
        .load(connection)
        .await?;

    Ok(holders.into_iter().flatten().collect())
}

/// The people holding the administrator role organization-wide, by account.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn organization_administrators(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    role_grants::table
        .inner_join(role_rows::table)
        .inner_join(
            organization_memberships::table.on(organization_memberships::id
                .nullable()
                .eq(role_grants::organization_membership_id)),
        )
        .filter(role_grants::organization_id.eq(organization_id))
        .filter(role_rows::key.eq(ADMIN_ROLE))
        .select(organization_memberships::user_id)
        .order(organization_memberships::user_id.asc())
        .load(connection)
        .await
}
