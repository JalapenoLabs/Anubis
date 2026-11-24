//! Who reaches a tenant, and what they may do there.
//!
//! This module is the one place that decides. A caller reaches an
//! organization, a sub-tenant, or a team along a handful of paths, and holds
//! runtime roles through grants hung off the memberships that gave the reach.
//! Two call sites that each worked the rules out for themselves would
//! eventually disagree about who may read something, and a disagreement
//! between two authorization paths is a data leak, so every guard and every
//! generated handler resolves through the three functions here, and none
//! re-derives them:
//!
//! | Function | Answers for |
//! |---|---|
//! | [`resolve_organization_access`] | The organization itself: its roster, its roles, its money |
//! | [`resolve_sub_tenant_access`] | One sub-tenant and the work it owns |
//! | [`resolve_team_access`] | One team, and optionally the records in hand |
//!
//! Each answers `None` for a tenant that does not exist and for one the caller
//! cannot reach, deliberately: a caller who can tell those apart can probe for
//! ids, so callers answer `404` for both.
//!
//! # Reach
//!
//! **A suspension is a deny.** It is checked before any grant and it outranks
//! every one of them, the administrator's bypass included: a deny an
//! administrator bit silently ignored would not be a deny. A suspended
//! organization membership cuts the member out of every tenant in the
//! organization; a suspended sub-tenant membership cuts them out of that one
//! and the teams scoped to it.
//!
//! Otherwise an organization is reached through an organization membership,
//! and a team through a claimed membership of it. A sub-tenant is reached when:
//!
//! 1. **The caller administers the organization.** An organization-wide
//!    administrator bypasses the tier and reaches every sub-tenant.
//! 2. **The caller is a full organization member.** Their standing cascades
//!    into every sub-tenant. A guest's does not: a guest reaches only what
//!    they were granted by name, which is the whole point of the distinction.
//! 3. **The caller holds a membership in this sub-tenant.** This is the
//!    explicit grant, and it applies to full members and guests alike, since
//!    it can only add.
//! 4. **The caller belongs to a team that reaches this sub-tenant.** An
//!    organization-level team (null `sub_tenant_id`) is inherited by every
//!    sub-tenant; a team scoped to one sub-tenant never leaks to another.
//!
//! # Roles
//!
//! What the caller may do is the union of the roles that apply where they
//! stand, and the organization's default role always applies. Which grants
//! apply is fixed per tier and mirrors the reach above:
//!
//! | Tier | Grants that apply |
//! |---|---|
//! | Organization | The organization membership's |
//! | Sub-tenant | The organization membership's when it cascades (rules 1 and 2), this sub-tenant membership's, and the team-wide grants of every team that reaches it |
//! | Team | The team membership's team-wide grants, and its grants on the records in hand |
//!
//! An organization-wide grant does not reach into a team: a team's roles are
//! the team's to grant. A grant on one record reaches that record and the
//! records nested under it, never the team. And in a sub-tenant, a team's
//! grants add their permissions but never rank: rank and the administrator
//! role there come from the organization and sub-tenant grants alone, since
//! administering one team is not administering the sub-tenant. So a narrow
//! grant can never raise anybody's standing in a wider place. Permission is
//! monotone in the roles held, so the union is exactly the strongest standing
//! the caller holds there, and rank (see [`Access::rank`]) is the highest of
//! the roles that rank there.

use std::collections::BTreeSet;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::http::ApiError;
use crate::roles::{ADMIN_ROLE, Action, DEFAULT_ROLE, Permission, RoleSet};
use crate::schema::{
    organization_memberships, organizations, role_grants, roles as role_rows,
    sub_tenant_memberships, sub_tenants, team_memberships, teams,
};
use crate::tenancy::model::{
    Organization, OrganizationAccess, OrganizationMembership, SubTenant, SubTenantMembership, Team,
    TeamMembership,
};
use crate::tenancy::role::{Role, RoleSummary};

/// What a caller may do in one place, resolved once from the roles there.
///
/// Holding one is the answer to "may this caller see this place at all", so a
/// handler that has one has already passed the tenancy boundary and asks only
/// about permissions. It is a snapshot: a handler that changes roles resolves
/// again under the organization's lock before trusting it.
#[derive(Debug, Clone)]
pub struct Access {
    /// The roles that apply, highest first, the default role last.
    roles: Vec<RoleSummary>,
    permissions: BTreeSet<Permission>,
    administrator: bool,
    rank: i32,
}

impl Access {
    /// Resolves the union of `held` under the compiled `roles`.
    ///
    /// The same role reached along two paths counts once.
    pub(crate) fn from_roles(held: Vec<Role>, roles: &RoleSet) -> Self {
        Self::ranked(held, Vec::new(), roles)
    }

    /// Resolves `ranking` and `narrower` together, ranked by `ranking` alone.
    ///
    /// Both add their permissions, but only `ranking` counts toward rank and
    /// the administrator role: the roles a narrower place granted reach here
    /// without making their holder senior here, which is what keeps a narrow
    /// grant from raising anybody's standing in a wider place.
    pub(crate) fn ranked(ranking: Vec<Role>, narrower: Vec<Role>, roles: &RoleSet) -> Self {
        let rank = ranking.iter().map(|role| role.position).max().unwrap_or(0);
        let administrator = ranking.iter().any(Role::is_administrator);

        let mut held = ranking;
        held.extend(narrower);
        held.sort_by_key(|role| std::cmp::Reverse(role.position));
        held.dedup_by_key(|role| role.id);

        let mut permissions = BTreeSet::new();
        for role in &held {
            permissions.extend(role.permissions(roles));
        }

        Self {
            roles: held.iter().map(|role| role.summary(roles)).collect(),
            permissions,
            administrator,
            rank,
        }
    }

    /// Returns `true` when a held role grants the action on the subject.
    #[must_use]
    pub fn can(&self, action: Action, subject: &str) -> bool {
        self.permissions
            .iter()
            .any(|permission| permission.allows(action, subject))
    }

    /// Rejects with `403` unless a held role grants the action.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, action: Action, subject: &str) -> Result<(), ApiError> {
        if self.can(action, subject) {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "You do not have permission to do that.",
            ))
        }
    }

    /// Rejects with `403` unless the caller holds the administrator role here.
    ///
    /// For what the framework keeps with the administrator rather than
    /// delegating: renaming or deleting a tenant, creating teams, the audit
    /// log, webhook endpoints, and platform applications. `doing` finishes
    /// "You need the administrator role to ...".
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] for anybody else.
    pub fn require_administrator(&self, doing: &str) -> Result<(), ApiError> {
        if self.administrator {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "You need the administrator role to {doing}."
            )))
        }
    }

    /// Returns `true` when the administrator role applies here.
    #[must_use]
    pub fn is_administrator(&self) -> bool {
        self.administrator
    }

    /// The position of the highest role that ranks here.
    ///
    /// The role hierarchy is measured with it: nobody edits, grants, or
    /// revokes a role at or above their own rank, and the administrator is
    /// exempt. The default role always applies, so the lowest rank is `0`.
    /// In a sub-tenant, a team's grants add permissions without ranking.
    #[must_use]
    pub fn rank(&self) -> i32 {
        self.rank
    }

    /// The roles that apply here, highest first.
    #[must_use]
    pub fn roles(&self) -> &[RoleSummary] {
        &self.roles
    }

    /// Every permission the roles here carry, sorted.
    #[must_use]
    pub fn permissions(&self) -> &BTreeSet<Permission> {
        &self.permissions
    }

    /// The access as the browser reads it, to draw only what it may use.
    #[must_use]
    pub fn view(&self) -> AccessView {
        AccessView {
            roles: self.roles.clone(),
            permissions: self.permissions.iter().map(ToString::to_string).collect(),
            rank: self.rank(),
            is_administrator: self.administrator,
        }
    }
}

/// What a caller may do somewhere, as `/tenancy` answers it.
#[derive(Debug, Clone, Serialize)]
pub struct AccessView {
    /// The roles that apply, highest first.
    pub roles: Vec<RoleSummary>,
    /// The permission keys those roles carry.
    pub permissions: Vec<String>,
    /// The position of the highest of them.
    pub rank: i32,
    /// Whether the administrator role applies.
    pub is_administrator: bool,
}

/// The strongest path by which a caller reached a sub-tenant, weakest first.
///
/// Ordering runs from the narrowest path to the broadest, so `max` picks the
/// strongest reason a caller is admitted. It explains a decision (to a person,
/// in a log, or in a screen that says why a control is there); it never makes
/// one, because the roles do that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// Through a team: one scoped to this sub-tenant, or an
    /// organization-level team that every sub-tenant inherits.
    Team,
    /// Through a membership in this sub-tenant, and nothing broader.
    Grant,
    /// Through a full organization membership, which cascades into every
    /// sub-tenant.
    Organization,
    /// Through the organization-wide administrator role, which bypasses the
    /// tier.
    Administrator,
}

/// A caller's resolved standing in one sub-tenant.
///
/// Produced only by [`resolve_sub_tenant_access`].
#[derive(Debug, Clone)]
pub struct SubTenantAccess {
    /// The caller's organization membership, when they hold one. A team-only
    /// member holds none, which the tenancy model allows.
    pub organization_membership: Option<OrganizationMembership>,
    /// The caller's explicit membership in this sub-tenant, when they hold one.
    pub membership: Option<SubTenantMembership>,
    /// What the roles that apply here allow.
    pub access: Access,
    /// The strongest path by which the caller was admitted.
    pub reach: Reach,
}

impl SubTenantAccess {
    /// Returns `true` when the caller administers the owning organization.
    #[must_use]
    pub fn is_organization_administrator(&self) -> bool {
        self.reach == Reach::Administrator
    }
}

/// Resolves a caller's standing in an organization, or `None` without one.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_organization_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    organization_id: Uuid,
) -> QueryResult<Option<(Organization, OrganizationMembership, Access)>> {
    let found: Option<(Organization, OrganizationMembership)> = organizations::table
        .inner_join(
            organization_memberships::table.on(organization_memberships::organization_id
                .eq(organizations::id)
                .and(organization_memberships::user_id.eq(user_id))),
        )
        .filter(organizations::id.eq(organization_id))
        // A suspension is a deny, not a missing grant.
        .filter(organization_memberships::suspended_at.is_null())
        .select((
            Organization::as_select(),
            OrganizationMembership::as_select(),
        ))
        .first(connection)
        .await
        .optional()?;
    let Some((organization, membership)) = found else {
        return Ok(None);
    };

    let held: Vec<Role> = role_rows::table
        .filter(role_rows::organization_id.eq(organization.id))
        .filter(
            role_rows::key.eq(DEFAULT_ROLE).or(role_rows::id.eq_any(
                role_grants::table
                    .filter(role_grants::organization_membership_id.eq(membership.id))
                    .select(role_grants::role_id),
            )),
        )
        .select(Role::as_select())
        .load(connection)
        .await?;

    let access = Access::from_roles(held, roles);
    Ok(Some((organization, membership, access)))
}

/// Resolves a caller's standing in a team, narrowed to the records in hand.
///
/// `resources` is the record a handler is about to act on and the records it
/// is nested under, nearest first, or empty for the team itself. A grant on
/// any of them applies, so a role granted on a record reaches everything
/// nested inside it.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_team_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    team_id: Uuid,
    resources: &[Uuid],
) -> QueryResult<Option<(Team, TeamMembership, Access)>> {
    let found: Option<(Team, TeamMembership)> = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(user_id))),
        )
        // A suspension is a deny, so it closes the team too: the
        // organization's cuts every team in it, and a sub-tenant's cuts the
        // teams scoped to that sub-tenant. Both joins are left joins filtered
        // on a null timestamp, so a member with no membership at the tier
        // above passes.
        .left_join(
            organization_memberships::table.on(organization_memberships::organization_id
                .eq(teams::organization_id)
                .and(organization_memberships::user_id.eq(user_id))),
        )
        .left_join(
            sub_tenant_memberships::table.on(sub_tenant_memberships::sub_tenant_id
                .nullable()
                .eq(teams::sub_tenant_id)
                .and(sub_tenant_memberships::user_id.eq(user_id))),
        )
        .filter(teams::id.eq(team_id))
        .filter(organization_memberships::suspended_at.is_null())
        .filter(sub_tenant_memberships::suspended_at.is_null())
        .select((Team::as_select(), TeamMembership::as_select()))
        .first(connection)
        .await
        .optional()?;
    let Some((team, membership)) = found else {
        return Ok(None);
    };

    let access = team_membership_access(connection, roles, &team, membership.id, resources).await?;
    Ok(Some((team, membership, access)))
}

/// The roles one team membership holds, claimed or not.
///
/// The team guard's answer for a caller, and the answer for whoever a
/// manager is about to act on, which includes an invited person who has not
/// signed up yet.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn team_membership_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    team: &Team,
    team_membership_id: Uuid,
    resources: &[Uuid],
) -> QueryResult<Access> {
    let held: Vec<Role> = role_rows::table
        .filter(role_rows::organization_id.eq(team.organization_id))
        .filter(
            role_rows::key.eq(DEFAULT_ROLE).or(role_rows::id.eq_any(
                role_grants::table
                    .filter(role_grants::team_membership_id.eq(team_membership_id))
                    .filter(
                        role_grants::resource_id
                            .is_null()
                            .or(role_grants::resource_id.eq_any(resources)),
                    )
                    .select(role_grants::role_id),
            )),
        )
        .select(Role::as_select())
        .load(connection)
        .await?;

    Ok(Access::from_roles(held, roles))
}

/// The roles one organization membership holds.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn organization_membership_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    organization_id: Uuid,
    organization_membership_id: Uuid,
) -> QueryResult<Access> {
    let held: Vec<Role> = role_rows::table
        .filter(role_rows::organization_id.eq(organization_id))
        .filter(
            role_rows::key.eq(DEFAULT_ROLE).or(role_rows::id.eq_any(
                role_grants::table
                    .filter(role_grants::organization_membership_id.eq(organization_membership_id))
                    .select(role_grants::role_id),
            )),
        )
        .select(Role::as_select())
        .load(connection)
        .await?;

    Ok(Access::from_roles(held, roles))
}

/// Resolves a caller's standing in a sub-tenant, or `None` when they have none.
///
/// The rules are the module's, and this is the only implementation of them.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_sub_tenant_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    sub_tenant_id: Uuid,
) -> QueryResult<Option<(SubTenant, SubTenantAccess)>> {
    let found: Option<SubTenant> = sub_tenants::table
        .find(sub_tenant_id)
        .select(SubTenant::as_select())
        .first(connection)
        .await
        .optional()?;
    let Some(sub_tenant) = found else {
        return Ok(None);
    };

    let organization_membership: Option<OrganizationMembership> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(sub_tenant.organization_id))
        .filter(organization_memberships::user_id.eq(user_id))
        .select(OrganizationMembership::as_select())
        .first(connection)
        .await
        .optional()?;

    let membership: Option<SubTenantMembership> = sub_tenant_memberships::table
        .filter(sub_tenant_memberships::sub_tenant_id.eq(sub_tenant.id))
        .filter(sub_tenant_memberships::user_id.eq(user_id))
        .select(SubTenantMembership::as_select())
        .first(connection)
        .await
        .optional()?;

    // The caller's memberships in the teams of this organization that reach
    // this sub-tenant: the organization-level ones every sub-tenant inherits,
    // and the ones scoped to this sub-tenant. A team scoped elsewhere is
    // filtered out here, which is what stops it leaking.
    let team_membership_ids: Vec<Uuid> = team_memberships::table
        .inner_join(teams::table)
        .filter(teams::organization_id.eq(sub_tenant.organization_id))
        .filter(
            teams::sub_tenant_id
                .is_null()
                .or(teams::sub_tenant_id.eq(sub_tenant.id)),
        )
        .filter(team_memberships::user_id.eq(user_id))
        .select(team_memberships::id)
        .load(connection)
        .await?;

    let organization_membership_id = organization_membership.as_ref().map(|held| held.id);
    let sub_tenant_membership_id = membership.as_ref().map(|held| held.id);
    // Every grant any of those memberships holds, each tagged with the tier it
    // came through: whether those apply here, and whether they rank here, is
    // the rules' decision, not the query's. Grants on single records are left
    // out, because a record is never the sub-tenant.
    let granted: Vec<(Option<Uuid>, Option<Uuid>, Role)> = role_grants::table
        .inner_join(role_rows::table)
        .filter(
            role_grants::organization_membership_id
                .eq(organization_membership_id)
                .or(role_grants::sub_tenant_membership_id.eq(sub_tenant_membership_id))
                .or(role_grants::team_membership_id
                    .eq_any(&team_membership_ids)
                    .and(role_grants::resource_id.is_null())),
        )
        .select((
            role_grants::organization_membership_id,
            role_grants::sub_tenant_membership_id,
            Role::as_select(),
        ))
        .load(connection)
        .await?;

    let default_role: Role = role_rows::table
        .filter(role_rows::organization_id.eq(sub_tenant.organization_id))
        .filter(role_rows::key.eq(DEFAULT_ROLE))
        .select(Role::as_select())
        .first(connection)
        .await?;

    let mut organization_roles = Vec::new();
    let mut sub_tenant_roles = Vec::new();
    let mut team_roles = Vec::new();
    for (through_organization, through_sub_tenant, role) in granted {
        match (through_organization, through_sub_tenant) {
            (Some(_membership), _) => organization_roles.push(role),
            (None, Some(_membership)) => sub_tenant_roles.push(role),
            (None, None) => team_roles.push(role),
        }
    }

    let standing = Standing {
        organization_membership: organization_membership.as_ref(),
        organization_roles,
        membership: membership.as_ref(),
        sub_tenant_roles,
        team_roles,
        team_member: !team_membership_ids.is_empty(),
    };
    let Some((reach, mut held)) = apply_sub_tenant_rules(standing) else {
        return Ok(None);
    };

    held.ranking.push(default_role);
    let access = SubTenantAccess {
        organization_membership,
        membership,
        access: held.access(roles),
        reach,
    };
    Ok(Some((sub_tenant, access)))
}

/// Everything a caller holds that could reach one sub-tenant.
struct Standing<'a> {
    organization_membership: Option<&'a OrganizationMembership>,
    /// Roles granted through the organization membership.
    organization_roles: Vec<Role>,
    membership: Option<&'a SubTenantMembership>,
    /// Roles granted through the sub-tenant membership.
    sub_tenant_roles: Vec<Role>,
    /// Roles granted team-wide in the teams that reach the sub-tenant.
    team_roles: Vec<Role>,
    /// Whether the caller belongs to a team that reaches the sub-tenant.
    team_member: bool,
}

/// The roles that apply in a sub-tenant, split by whether they rank there.
struct SubTenantRoles {
    /// Organization-wide grants that cascade here, and this sub-tenant's own.
    ranking: Vec<Role>,
    /// Team-wide grants of the teams that reach here: they add permissions,
    /// but administering one team is not administering the sub-tenant.
    narrower: Vec<Role>,
}

impl SubTenantRoles {
    fn access(self, roles: &RoleSet) -> Access {
        Access::ranked(self.ranking, self.narrower, roles)
    }
}

/// Applies the module's rules to what a caller holds near one sub-tenant.
///
/// Split out from the queries so the rules are testable without a database,
/// and so the resolver reads as "gather the grants" and this as "apply them".
/// Returns the reach and the roles that apply, the default role aside.
fn apply_sub_tenant_rules(standing: Standing<'_>) -> Option<(Reach, SubTenantRoles)> {
    let suspended = standing
        .organization_membership
        .is_some_and(|held| held.suspended_at.is_some())
        || standing
            .membership
            .is_some_and(|held| held.suspended_at.is_some());
    if suspended {
        return None;
    }

    let mut reach = None;
    let mut ranking = Vec::new();

    if let Some(organization_membership) = standing.organization_membership {
        let administrator = standing
            .organization_roles
            .iter()
            .any(|role| role.key.as_deref() == Some(ADMIN_ROLE));
        if administrator || organization_membership.access == OrganizationAccess::Full {
            reach = reach.max(Some(if administrator {
                Reach::Administrator
            } else {
                Reach::Organization
            }));
            ranking.extend(standing.organization_roles);
        }
    }

    if standing.membership.is_some() {
        reach = reach.max(Some(Reach::Grant));
    }

    if standing.team_member {
        // Reach comes from belonging to the team, not from what the membership
        // grants, so a role-less team member still reaches the sub-tenant.
        reach = reach.max(Some(Reach::Team));
    }

    // No path applied, so the caller has no standing here at all.
    let reach = reach?;

    // Only memberships that reach the sub-tenant were asked about, so every
    // one of these applies.
    ranking.extend(standing.sub_tenant_roles);
    Some((
        reach,
        SubTenantRoles {
            ranking,
            narrower: standing.team_roles,
        },
    ))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{Access, Reach, Standing, apply_sub_tenant_rules};
    use crate::roles::{Action, RoleSet};
    use crate::tenancy::model::{OrganizationAccess, OrganizationMembership, SubTenantMembership};
    use crate::tenancy::role::Role;

    const ROLES: &str = "
roles:
  default:
    models:
      Project: [read]
  editor:
    includes: [default]
    models:
      Project: [manage]
  admin:
    includes: [editor]
";

    fn role(key: &str, position: i32) -> Role {
        Role {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            key: Some(key.to_owned()),
            name: None,
            color: None,
            position,
            permissions: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn organization_membership(access: OrganizationAccess) -> OrganizationMembership {
        OrganizationMembership {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            access,
            suspended_at: None,
        }
    }

    fn sub_tenant_membership() -> SubTenantMembership {
        SubTenantMembership {
            id: Uuid::new_v4(),
            sub_tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            suspended_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn keys(held: &[Role]) -> Vec<&str> {
        let mut keys: Vec<&str> = held.iter().filter_map(|role| role.key.as_deref()).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn a_full_member_cascades_into_the_sub_tenant() {
        let organization = organization_membership(OrganizationAccess::Full);
        let (reach, held) = apply_sub_tenant_rules(Standing {
            organization_membership: Some(&organization),
            organization_roles: vec![role("editor", 1)],
            membership: None,
            sub_tenant_roles: Vec::new(),
            team_roles: Vec::new(),
            team_member: false,
        })
        .expect("a full member reaches every sub-tenant");

        assert_eq!(reach, Reach::Organization);
        assert_eq!(keys(&held.ranking), ["editor"]);
    }

    #[test]
    fn an_organization_administrator_bypasses_the_tier() {
        let organization = organization_membership(OrganizationAccess::Full);
        let (reach, _held) = apply_sub_tenant_rules(Standing {
            organization_membership: Some(&organization),
            organization_roles: vec![role("admin", 2)],
            membership: None,
            sub_tenant_roles: Vec::new(),
            team_roles: Vec::new(),
            team_member: false,
        })
        .expect("an administrator reaches every sub-tenant");

        assert_eq!(reach, Reach::Administrator);
    }

    #[test]
    fn an_administrator_marked_as_a_guest_still_bypasses() {
        // Guest and administrator together is a contradiction an administrator
        // can write; the administrator role wins, and saying so beats leaving
        // it to whichever branch happened to run first.
        let organization = organization_membership(OrganizationAccess::Guest);
        let (reach, _held) = apply_sub_tenant_rules(Standing {
            organization_membership: Some(&organization),
            organization_roles: vec![role("admin", 2)],
            membership: None,
            sub_tenant_roles: Vec::new(),
            team_roles: Vec::new(),
            team_member: false,
        })
        .expect("the administrator role outranks the guest marker");

        assert_eq!(reach, Reach::Administrator);
    }

    #[test]
    fn a_guest_without_a_grant_reaches_nothing() {
        let organization = organization_membership(OrganizationAccess::Guest);
        assert!(
            apply_sub_tenant_rules(Standing {
                organization_membership: Some(&organization),
                organization_roles: vec![role("editor", 1)],
                membership: None,
                sub_tenant_roles: Vec::new(),
                team_roles: Vec::new(),
                team_member: false,
            })
            .is_none(),
            "a guest reaches only what was granted by name",
        );
    }

    #[test]
    fn a_guest_holds_only_the_roles_granted_by_name() {
        let organization = organization_membership(OrganizationAccess::Guest);
        let membership = sub_tenant_membership();
        let (reach, held) = apply_sub_tenant_rules(Standing {
            organization_membership: Some(&organization),
            organization_roles: vec![role("editor", 1)],
            membership: Some(&membership),
            sub_tenant_roles: Vec::new(),
            team_roles: Vec::new(),
            team_member: false,
        })
        .expect("an explicit grant admits a guest");

        assert_eq!(reach, Reach::Grant);
        assert!(
            held.ranking.is_empty() && held.narrower.is_empty(),
            "the organization role must not cascade to a guest",
        );
    }

    #[test]
    fn a_team_alone_admits_a_user_with_no_other_standing() {
        // A team-only invitation produces exactly this state: no organization
        // membership at all, and a team that reaches the sub-tenant.
        let (reach, held) = apply_sub_tenant_rules(Standing {
            organization_membership: None,
            organization_roles: Vec::new(),
            membership: None,
            sub_tenant_roles: Vec::new(),
            team_roles: Vec::new(),
            team_member: true,
        })
        .expect("a team member reaches the sub-tenant");

        assert_eq!(reach, Reach::Team);
        assert!(
            held.ranking.is_empty() && held.narrower.is_empty(),
            "reach comes from belonging, not from the roles held",
        );
    }

    #[test]
    fn a_suspension_outranks_every_grant() {
        let mut suspended = organization_membership(OrganizationAccess::Full);
        suspended.suspended_at = Some(Utc::now());
        let membership = sub_tenant_membership();
        assert!(
            apply_sub_tenant_rules(Standing {
                organization_membership: Some(&suspended),
                organization_roles: vec![role("admin", 2)],
                membership: Some(&membership),
                sub_tenant_roles: vec![role("editor", 1)],
                team_roles: vec![role("editor", 1)],
                team_member: true,
            })
            .is_none(),
            "an organization suspension cuts the member out entirely",
        );

        let organization = organization_membership(OrganizationAccess::Full);
        let mut suspended_here = sub_tenant_membership();
        suspended_here.suspended_at = Some(Utc::now());
        assert!(
            apply_sub_tenant_rules(Standing {
                organization_membership: Some(&organization),
                organization_roles: vec![role("admin", 2)],
                membership: Some(&suspended_here),
                sub_tenant_roles: vec![role("editor", 1)],
                team_roles: vec![role("editor", 1)],
                team_member: true,
            })
            .is_none(),
            "a sub-tenant suspension cuts the member out of this sub-tenant",
        );
    }

    #[test]
    fn a_stranger_reaches_nothing() {
        assert!(
            apply_sub_tenant_rules(Standing {
                organization_membership: None,
                organization_roles: Vec::new(),
                membership: None,
                sub_tenant_roles: Vec::new(),
                team_roles: Vec::new(),
                team_member: false,
            })
            .is_none()
        );
    }

    #[test]
    fn a_team_grant_adds_permissions_in_a_sub_tenant_but_never_rank() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");
        let organization = organization_membership(OrganizationAccess::Full);
        let (_reach, held) = apply_sub_tenant_rules(Standing {
            organization_membership: Some(&organization),
            organization_roles: vec![role("default", 0)],
            membership: None,
            sub_tenant_roles: Vec::new(),
            team_roles: vec![role("admin", 2)],
            team_member: true,
        })
        .expect("a full member reaches the sub-tenant");
        let access = held.access(&roles);

        assert!(
            access.can(Action::Destroy, "Project"),
            "the team's grants still add what they carry",
        );
        assert!(
            !access.is_administrator(),
            "administering one team is not administering the sub-tenant",
        );
        assert_eq!(access.rank(), 0, "a team grant never raises rank here");
    }

    #[test]
    fn access_is_the_union_ranked_by_the_highest_role() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");
        let editor = role("editor", 1);
        let access = Access::from_roles(vec![role("default", 0), editor.clone(), editor], &roles);

        assert_eq!(access.rank(), 1);
        assert_eq!(
            access.roles().len(),
            2,
            "one role reached twice counts once"
        );
        assert!(access.can(Action::Destroy, "Project"));
        assert!(!access.is_administrator());

        let lowest = Access::from_roles(vec![role("default", 0)], &roles);
        assert_eq!(lowest.rank(), 0);
        assert!(lowest.can(Action::Read, "Project"));
        assert!(!lowest.can(Action::Update, "Project"));
    }
}
