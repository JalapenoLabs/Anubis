//! Who reaches a sub-tenant, and with which roles.
//!
//! A user can reach a sub-tenant along four paths, and this module is the one
//! place that reconciles them. Two call sites that each worked the rules out
//! for themselves would eventually disagree about who may read something, and
//! a disagreement between two authorization paths is a data leak, so every
//! caller resolves through [`resolve_sub_tenant_access`] and none re-derives
//! it. The [`SubTenantMember`](crate::guard::SubTenantMember) guard is the
//! first caller; application handlers are the rest.
//!
//! # The rules
//!
//! **A suspension is a deny.** It is checked before any grant and it outranks
//! every one of them, the organization admin's bypass included: a deny an
//! admin bit silently ignored would not be a deny. A suspended organization
//! membership cuts the member out of every sub-tenant in the organization; a
//! suspended sub-tenant membership cuts them out of that one.
//!
//! Otherwise a grant applies when:
//!
//! 1. **The caller administers the organization.** An admin bypasses the
//!    tier and reaches every sub-tenant in it.
//! 2. **The caller is a full organization member.** Their standing cascades
//!    into every sub-tenant. A guest's does not: a guest reaches only what
//!    they were granted by name, which is the whole point of the distinction.
//! 3. **The caller holds a membership in this sub-tenant.** This is the
//!    explicit grant, and it applies to full members and guests alike, since
//!    it can only add.
//! 4. **The caller belongs to a team that reaches this sub-tenant.** An
//!    organization-level team (null `sub_tenant_id`) is inherited by every
//!    sub-tenant; a team scoped to one sub-tenant never leaks to another.
//!    A team membership grants reach whatever the member's organization
//!    access says, because putting somebody on a team is itself the explicit
//!    act: an administrator confining a guest to one sub-tenant scopes the
//!    team to it rather than leaving the team organization-level.
//!
//! The resolved role set is the **union** of every applying grant's roles.
//! Permission is monotone in the role set ([`RoleSet::can`] asks whether *any*
//! held role grants the action), so the union is exactly the strongest
//! standing the caller holds across the tiers.
//!
//! [`RoleSet::can`]: crate::roles::RoleSet::can

use std::collections::BTreeSet;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::{
    organization_memberships, sub_tenant_memberships, sub_tenants, team_memberships, teams,
};
use crate::tenancy::bootstrap::holds_admin;
use crate::tenancy::model::{
    OrganizationAccess, OrganizationMembership, SubTenant, SubTenantMembership,
};

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
    /// Through the organization's `admin` role, which bypasses the tier.
    Administrator,
}

/// A caller's resolved standing in one sub-tenant.
///
/// Produced only by [`resolve_sub_tenant_access`]; its existence is itself the
/// answer to "may this caller see this sub-tenant at all", so a handler that
/// holds one has already passed the tenancy boundary and needs only to ask
/// about permissions.
#[derive(Debug, Clone)]
pub struct SubTenantAccess {
    /// The caller's organization membership, when they hold one. A team-only
    /// member holds none, which the tenancy model allows.
    pub organization_membership: Option<OrganizationMembership>,
    /// The caller's explicit membership in this sub-tenant, when they hold one.
    pub membership: Option<SubTenantMembership>,
    /// Every role key that applies here, deduplicated and sorted.
    pub roles: Vec<String>,
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

/// Resolves a caller's standing in a sub-tenant, or `None` when they have none.
///
/// `None` covers both a sub-tenant that does not exist and one the caller
/// cannot reach, deliberately: a caller who can tell those apart can probe for
/// ids. Callers answer `404` for either.
///
/// The rules are the module's, and this is the only implementation of them.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_sub_tenant_access(
    connection: &mut AsyncPgConnection,
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

    // The teams of this organization that reach this sub-tenant: the
    // organization-level ones every sub-tenant inherits, and the ones scoped
    // to this sub-tenant. A team scoped elsewhere is filtered out here, which
    // is what stops it leaking.
    let team_roles: Vec<Vec<String>> = team_memberships::table
        .inner_join(teams::table)
        .filter(teams::organization_id.eq(sub_tenant.organization_id))
        .filter(
            teams::sub_tenant_id
                .is_null()
                .or(teams::sub_tenant_id.eq(sub_tenant.id)),
        )
        .filter(team_memberships::user_id.eq(user_id))
        .select(team_memberships::roles)
        .load(connection)
        .await?;

    Ok(grant(organization_membership, membership, team_roles).map(|access| (sub_tenant, access)))
}

/// Applies the module's rules to the grants a caller holds.
///
/// Split out from the query so the rules are testable without a database, and
/// so the query above reads as "gather the grants" and this as "apply them".
fn grant(
    organization_membership: Option<OrganizationMembership>,
    membership: Option<SubTenantMembership>,
    team_roles: Vec<Vec<String>>,
) -> Option<SubTenantAccess> {
    let suspended = organization_membership
        .as_ref()
        .is_some_and(|held| held.suspended_at.is_some())
        || membership
            .as_ref()
            .is_some_and(|held| held.suspended_at.is_some());
    if suspended {
        return None;
    }

    // A set rather than a vector: two tiers naming the same role must not
    // double it, and sorted output makes the standing comparable and loggable.
    let mut roles = BTreeSet::new();
    let mut reach = None;

    if let Some(held) = &organization_membership {
        let administrator = holds_admin(&held.roles);
        if administrator || held.access == OrganizationAccess::Full {
            roles.extend(held.roles.iter().cloned());
            reach = reach.max(Some(if administrator {
                Reach::Administrator
            } else {
                Reach::Organization
            }));
        }
    }

    if let Some(held) = &membership {
        roles.extend(held.roles.iter().cloned());
        reach = reach.max(Some(Reach::Grant));
    }

    if !team_roles.is_empty() {
        // Reach comes from belonging to the team, not from what the membership
        // grants, so a role-less team member still reaches the sub-tenant.
        reach = reach.max(Some(Reach::Team));
        roles.extend(team_roles.into_iter().flatten());
    }

    // No path applied, so the caller has no standing here at all.
    let reach = reach?;

    Some(SubTenantAccess {
        organization_membership,
        membership,
        roles: roles.into_iter().collect(),
        reach,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{Reach, grant};
    use crate::tenancy::model::{OrganizationAccess, OrganizationMembership, SubTenantMembership};

    fn organization_membership(
        access: OrganizationAccess,
        roles: &[&str],
    ) -> OrganizationMembership {
        OrganizationMembership {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            access,
            suspended_at: None,
        }
    }

    fn sub_tenant_membership(roles: &[&str]) -> SubTenantMembership {
        SubTenantMembership {
            id: Uuid::new_v4(),
            sub_tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            suspended_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn roles(names: &[&str]) -> Vec<Vec<String>> {
        vec![names.iter().map(|name| (*name).to_owned()).collect()]
    }

    #[test]
    fn a_full_member_cascades_into_the_sub_tenant() {
        let access = grant(
            Some(organization_membership(
                OrganizationAccess::Full,
                &["editor"],
            )),
            None,
            Vec::new(),
        )
        .expect("a full member reaches every sub-tenant");

        assert_eq!(access.reach, Reach::Organization);
        assert_eq!(access.roles, vec!["editor".to_owned()]);
    }

    #[test]
    fn an_organization_admin_bypasses_the_tier() {
        let access = grant(
            Some(organization_membership(
                OrganizationAccess::Full,
                &["admin"],
            )),
            None,
            Vec::new(),
        )
        .expect("an admin reaches every sub-tenant");

        assert_eq!(access.reach, Reach::Administrator);
        assert!(access.is_organization_administrator());
    }

    #[test]
    fn an_admin_marked_as_a_guest_still_bypasses() {
        // Guest and admin together is a contradiction an administrator can
        // write; admin wins, and saying so beats leaving it to whichever
        // branch happened to run first.
        let access = grant(
            Some(organization_membership(
                OrganizationAccess::Guest,
                &["admin"],
            )),
            None,
            Vec::new(),
        )
        .expect("the admin role outranks the guest marker");

        assert_eq!(access.reach, Reach::Administrator);
    }

    #[test]
    fn a_guest_without_a_grant_reaches_nothing() {
        assert!(
            grant(
                Some(organization_membership(
                    OrganizationAccess::Guest,
                    &["editor"],
                )),
                None,
                Vec::new(),
            )
            .is_none(),
            "a guest reaches only what was granted by name",
        );
    }

    #[test]
    fn a_guest_holds_only_the_roles_granted_by_name() {
        let access = grant(
            Some(organization_membership(
                OrganizationAccess::Guest,
                &["editor"],
            )),
            Some(sub_tenant_membership(&["default"])),
            Vec::new(),
        )
        .expect("an explicit grant admits a guest");

        assert_eq!(access.reach, Reach::Grant);
        assert_eq!(
            access.roles,
            vec!["default".to_owned()],
            "the organization role must not cascade to a guest",
        );
    }

    #[test]
    fn a_team_alone_admits_a_user_with_no_other_standing() {
        // A team-only invitation produces exactly this state: no organization
        // membership at all, and a team that reaches the sub-tenant.
        let access = grant(None, None, roles(&[])).expect("a team member reaches the sub-tenant");

        assert_eq!(access.reach, Reach::Team);
        assert!(
            access.roles.is_empty(),
            "reach comes from belonging, not from the roles held",
        );
    }

    #[test]
    fn roles_are_the_deduplicated_union_of_every_grant() {
        let access = grant(
            Some(organization_membership(
                OrganizationAccess::Full,
                &["default"],
            )),
            Some(sub_tenant_membership(&["editor", "default"])),
            roles(&["billing"]),
        )
        .expect("every path admits the caller");

        assert_eq!(
            access.roles,
            vec![
                "billing".to_owned(),
                "default".to_owned(),
                "editor".to_owned()
            ],
        );
        assert_eq!(
            access.reach,
            Reach::Organization,
            "the strongest path wins the explanation",
        );
    }

    #[test]
    fn a_suspension_outranks_every_grant() {
        let mut suspended = organization_membership(OrganizationAccess::Full, &["admin"]);
        suspended.suspended_at = Some(Utc::now());
        assert!(
            grant(
                Some(suspended),
                Some(sub_tenant_membership(&["editor"])),
                roles(&["editor"])
            )
            .is_none(),
            "an organization suspension cuts the member out entirely",
        );

        let mut suspended_here = sub_tenant_membership(&["editor"]);
        suspended_here.suspended_at = Some(Utc::now());
        assert!(
            grant(
                Some(organization_membership(
                    OrganizationAccess::Full,
                    &["admin"],
                )),
                Some(suspended_here),
                roles(&["editor"]),
            )
            .is_none(),
            "a sub-tenant suspension cuts the member out of this sub-tenant",
        );
    }

    #[test]
    fn a_stranger_reaches_nothing() {
        assert!(grant(None, None, Vec::new()).is_none());
    }
}
