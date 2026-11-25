//! Who reaches the records an organization owns directly, and what they may do.
//!
//! Most records belong to a team and are reached through it. Some belong to
//! the organization itself (a fleet of machines, a shared catalog) and are
//! shared with its teams by choice: teams A and B reach every machine, team C
//! only three of them. A **resource grant** is that choice, one row giving one
//! team every record of a kind in the organization, or one record of it. This
//! module is the one place that decides what those grants, and the caller's
//! organization standing, add up to; the guards an application writes for
//! such records and the token surface both ask it, and none re-derives it.
//!
//! # Reach
//!
//! **A suspension is a deny**, checked first, as everywhere: a suspended
//! organization membership reaches nothing in the organization, and a team
//! the caller is suspended from (through its sub-tenant) shares nothing with
//! them. Otherwise an organization's record of kind `K` is reached when:
//!
//! 1. **The caller administers the organization.** The administrator role,
//!    granted organization-wide, reaches every record the organization owns.
//! 2. **An organization-wide grant reads the kind.** A full member holding,
//!    through a role granted organization-wide, `K.read` reaches every record
//!    of `K`. The default role is not a grant and reaches nothing by itself,
//!    because it is everybody's: were it a way in, every member would reach
//!    every record, and no team could be held to three machines.
//! 3. **One of the caller's teams was granted the record.** A claimed member
//!    of a team granted every record of `K`, or this record, reaches it.
//!
//! A guest's organization standing reaches nothing here, as it reaches no
//! sub-tenant: a guest reaches what a team they were put on was granted.
//!
//! # Roles
//!
//! On a record the caller reaches, the roles that apply are the union of the
//! default role, the organization-wide grants (for a full member or an
//! administrator, since the record is the organization's own surface), and
//! the team-wide grants of every team of theirs granted the record. **A team's
//! grants add permissions but never rank**, exactly as in a sub-tenant: the
//! record's rank and administrator role come from the organization-wide grants
//! alone, so reach through a team never makes anybody senior anywhere.
//!
//! # Tokens
//!
//! A platform application's token acts as its team, with the compiled `admin`
//! grants, so it reaches the records its team was granted and nothing else
//! ([`team_reaches_resource`], [`team_resource_scope`]). That is why sharing a
//! kind takes every permission on it a tenant may hold: see
//! `docs/tenancy.md#resource-grants`.
//!
//! # Records that go away
//!
//! A grant names an application record by id and is never a foreign key, so
//! deleting the record leaves the grant behind. It is inert: access is only
//! ever resolved with a record in hand, and a record id is never issued twice.
//! An application's delete still calls [`forget_resource`] in its own
//! transaction, after the delete, so nothing lingers in the access screen.

use std::collections::{BTreeMap, BTreeSet};

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::roles::{Action, DEFAULT_ROLE, Permission, RoleSet};
use crate::schema::{
    organization_memberships, resource_grants, role_grants, roles as role_rows, team_memberships,
    teams,
};
use crate::tenancy::access::{Access, admitted_teams};
use crate::tenancy::model::{OrganizationAccess, OrganizationMembership, Team};
use crate::tenancy::role::Role;

/// One record an organization owns directly, as an application names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrganizationResource<'a> {
    /// The model the record is, as `roles.yml` names it.
    pub kind: &'a str,
    /// The organization that owns it, read off the record's own row.
    pub organization_id: Uuid,
    /// The record's id.
    pub id: Uuid,
}

/// Which records of one kind, in one organization, a caller may act on.
///
/// Answered for a list: an application filters its own query by the
/// organization **and** by this, because [`ResourceScope::Every`] means every
/// record of the kind in that organization, never in any other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceScope {
    /// Every record of the kind the organization owns, now and later.
    Every,
    /// Exactly these records, which may be none.
    Only(BTreeSet<Uuid>),
}

impl ResourceScope {
    /// Returns `true` when the scope takes in `resource_id`.
    #[must_use]
    pub fn includes(&self, resource_id: Uuid) -> bool {
        match self {
            Self::Every => true,
            Self::Only(ids) => ids.contains(&resource_id),
        }
    }

    /// Widens `self` to take in `other` too.
    fn extend(&mut self, other: &Self) {
        match (self, other) {
            (Self::Every, _other) => {}
            (scope, Self::Every) => *scope = Self::Every,
            (Self::Only(ids), Self::Only(more)) => ids.extend(more),
        }
    }
}

/// Resolves a caller's standing on one record an organization owns.
///
/// `None` for a record the caller does not reach, which a handler answers
/// `404` exactly as it answers a record that does not exist. The record's
/// organization must come from the record's own row, never from the request.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_resource_access(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    resource: OrganizationResource<'_>,
) -> QueryResult<Option<Access>> {
    let Some(standing) = gather(
        connection,
        user_id,
        resource.organization_id,
        resource.kind,
        Some(resource.id),
    )
    .await?
    else {
        return Ok(None);
    };

    // Only grants covering this record were gathered, so every team here
    // shares it with the caller.
    let narrower = standing.teams.into_iter().map(|team| team.roles).collect();
    let policy = Rules {
        organization: standing.organization.as_ref(),
        kind: resource.kind,
        roles,
    };
    Ok(policy.on_record(narrower).map(|(mut ranking, narrower)| {
        ranking.push(standing.default_role);
        Access::ranked(ranking, narrower, roles)
    }))
}

/// Resolves which records of `kind` in an organization a caller may `action`.
///
/// `None` when the caller has no standing there at all: no organization
/// membership and no team granted any record of the kind, or a suspension.
/// An application's list route answers that `404`, like an unreachable
/// tenant, and filters by the scope otherwise.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub async fn resolve_resource_scope(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    organization_id: Uuid,
    kind: &str,
    action: Action,
) -> QueryResult<Option<ResourceScope>> {
    let Some(standing) = gather(connection, user_id, organization_id, kind, None).await? else {
        return Ok(None);
    };
    if standing.organization.is_none() && standing.teams.is_empty() {
        return Ok(None);
    }

    let policy = Rules {
        organization: standing.organization.as_ref(),
        kind,
        roles,
    };
    Ok(Some(policy.scope(
        standing.teams,
        standing.default_role,
        action,
    )))
}

/// Returns `true` when `team` was granted `resource`.
///
/// The token surface's question: a platform application acts as its team, so
/// its reach is exactly the team's grants, in the team's own organization.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub async fn team_reaches_resource(
    connection: &mut AsyncPgConnection,
    team: &Team,
    resource: OrganizationResource<'_>,
) -> QueryResult<bool> {
    // A team reaches nothing another organization owns, whatever it holds.
    if team.organization_id != resource.organization_id {
        return Ok(false);
    }

    diesel::select(diesel::dsl::exists(
        resource_grants::table
            .filter(resource_grants::team_id.eq(team.id))
            .filter(resource_grants::organization_id.eq(resource.organization_id))
            .filter(resource_grants::kind.eq(resource.kind))
            .filter(
                resource_grants::resource_id
                    .is_null()
                    .or(resource_grants::resource_id.eq(resource.id)),
            ),
    ))
    .get_result(connection)
    .await
}

/// The records of `kind` in its own organization that `team` was granted.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub async fn team_resource_scope(
    connection: &mut AsyncPgConnection,
    team: &Team,
    kind: &str,
) -> QueryResult<ResourceScope> {
    let granted: Vec<Option<Uuid>> = resource_grants::table
        .filter(resource_grants::team_id.eq(team.id))
        .filter(resource_grants::organization_id.eq(team.organization_id))
        .filter(resource_grants::kind.eq(kind))
        .select(resource_grants::resource_id)
        .load(connection)
        .await?;
    Ok(coverage(granted))
}

/// Removes every grant naming one record, for the record's own delete.
///
/// Call it inside the transaction that deletes the record, after the delete:
/// a grant being written at the same instant holds the record's row
/// `FOR KEY SHARE` (see [`ResourceKind::find`]), so the delete waits for it
/// and this then sees it. Returns how many grants went.
///
/// [`ResourceKind::find`]: crate::tenancy::ResourceKind::find
///
/// # Errors
/// Returns the underlying Diesel error when the delete fails.
pub async fn forget_resource(
    connection: &mut AsyncPgConnection,
    kind: &str,
    resource_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        resource_grants::table
            .filter(resource_grants::kind.eq(kind))
            .filter(resource_grants::resource_id.eq(resource_id)),
    )
    .execute(connection)
    .await
}

/// The caller's organization membership, and the roles granted through it.
struct OrganizationStanding {
    access: OrganizationAccess,
    /// Organization-wide grants, the default role aside: it is nobody's grant.
    granted: Vec<Role>,
}

/// One of the caller's teams that shares records of the kind with them.
struct TeamStanding {
    /// The team-wide grants of the caller's membership in it.
    roles: Vec<Role>,
    /// Which records of the kind the team was granted.
    coverage: ResourceScope,
}

/// Everything the caller holds near one organization's records of one kind.
struct Standing {
    organization: Option<OrganizationStanding>,
    teams: Vec<TeamStanding>,
    default_role: Role,
}

/// A caller's organization-wide standing over an organization's own records.
///
/// The organization half of what [`gather`] reads, and on its own what the
/// routes that share those records are held to: only somebody whose
/// organization standing reaches every record of a kind chooses which teams
/// reach them.
pub(crate) struct OrganizationReach {
    organization: Option<OrganizationStanding>,
    default_role: Role,
}

impl OrganizationReach {
    /// Reads the caller's organization membership and its grants.
    ///
    /// `None` is a deny: the membership is suspended, or the organization does
    /// not exist. A caller with no membership at all reads as one, holding
    /// nothing organization-wide.
    pub(crate) async fn read(
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        organization_id: Uuid,
    ) -> QueryResult<Option<Self>> {
        let default_role: Option<Role> = role_rows::table
            .filter(role_rows::organization_id.eq(organization_id))
            .filter(role_rows::key.eq(DEFAULT_ROLE))
            .select(Role::as_select())
            .first(connection)
            .await
            .optional()?;
        // Every organization holds a default role, so none means no organization.
        let Some(default_role) = default_role else {
            return Ok(None);
        };

        let membership: Option<OrganizationMembership> = organization_memberships::table
            .filter(organization_memberships::organization_id.eq(organization_id))
            .filter(organization_memberships::user_id.eq(user_id))
            .select(OrganizationMembership::as_select())
            .first(connection)
            .await
            .optional()?;
        // A suspension is a deny, not a missing grant: it outranks every path.
        if membership
            .as_ref()
            .is_some_and(|held| held.suspended_at.is_some())
        {
            return Ok(None);
        }

        let organization = match membership {
            Some(membership) => {
                let granted: Vec<Role> = role_grants::table
                    .inner_join(role_rows::table)
                    .filter(role_grants::organization_membership_id.eq(membership.id))
                    .select(Role::as_select())
                    .load(connection)
                    .await?;
                Some(OrganizationStanding {
                    access: membership.access,
                    granted,
                })
            }
            None => None,
        };

        Ok(Some(Self {
            organization,
            default_role,
        }))
    }

    /// The organization-wide access on every record of `kind`, when the
    /// caller's organization standing alone reaches them all.
    pub(crate) fn on_every(&self, kind: &str, roles: &RoleSet) -> Option<Access> {
        let policy = Rules {
            organization: self.organization.as_ref(),
            kind,
            roles,
        };
        if !policy.organization_reaches() {
            return None;
        }
        let mut ranking = policy.ranking();
        ranking.push(self.default_role.clone());
        Some(Access::ranked(ranking, Vec::new(), roles))
    }
}

/// Reads what the caller holds near `organization_id`'s records of `kind`.
///
/// `resource_id` narrows the team side to grants covering that record; `None`
/// reads every grant of the kind, for a list. `None` back is a deny, as
/// [`OrganizationReach::read`] says.
async fn gather(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    organization_id: Uuid,
    kind: &str,
    resource_id: Option<Uuid>,
) -> QueryResult<Option<Standing>> {
    let Some(reach) = OrganizationReach::read(connection, user_id, organization_id).await? else {
        return Ok(None);
    };

    // The caller's teams here that were granted records of the kind. The
    // admitted-teams statement is the one every team route reads, so a claimed
    // membership admits, and a suspension through the team's sub-tenant
    // denies, exactly as they do there.
    let mut grants = admitted_teams(user_id)
        .inner_join(resource_grants::table.on(resource_grants::team_id.eq(teams::id)))
        .filter(teams::organization_id.eq(organization_id))
        .filter(resource_grants::organization_id.eq(organization_id))
        .filter(resource_grants::kind.eq(kind))
        .select((team_memberships::id, resource_grants::resource_id))
        .into_boxed();
    if let Some(resource_id) = resource_id {
        grants = grants.filter(
            resource_grants::resource_id
                .is_null()
                .or(resource_grants::resource_id.eq(resource_id)),
        );
    }
    let grants: Vec<(Uuid, Option<Uuid>)> = grants.load(connection).await?;

    let mut covered: BTreeMap<Uuid, Vec<Option<Uuid>>> = BTreeMap::new();
    for (team_membership_id, granted) in grants {
        covered.entry(team_membership_id).or_default().push(granted);
    }

    let team_membership_ids: Vec<Uuid> = covered.keys().copied().collect();
    let team_roles: Vec<(Option<Uuid>, Role)> = role_grants::table
        .inner_join(role_rows::table)
        .filter(role_grants::team_membership_id.eq_any(&team_membership_ids))
        // A grant on one record of a team is about that team's own records,
        // never about the organization's.
        .filter(role_grants::resource_id.is_null())
        .select((role_grants::team_membership_id, Role::as_select()))
        .load(connection)
        .await?;

    let mut roles_by_membership: BTreeMap<Uuid, Vec<Role>> = BTreeMap::new();
    for (team_membership_id, role) in team_roles {
        if let Some(team_membership_id) = team_membership_id {
            roles_by_membership
                .entry(team_membership_id)
                .or_default()
                .push(role);
        }
    }

    let teams = covered
        .into_iter()
        .map(|(team_membership_id, granted)| TeamStanding {
            roles: roles_by_membership
                .remove(&team_membership_id)
                .unwrap_or_default(),
            coverage: coverage(granted),
        })
        .collect();

    Ok(Some(Standing {
        organization: reach.organization,
        teams,
        default_role: reach.default_role,
    }))
}

/// What a team's grants of one kind take in: every record once any grant
/// names none, otherwise the records they name.
fn coverage(granted: Vec<Option<Uuid>>) -> ResourceScope {
    let mut ids = BTreeSet::new();
    for resource_id in granted {
        match resource_id {
            Some(id) => {
                ids.insert(id);
            }
            None => return ResourceScope::Every,
        }
    }
    ResourceScope::Only(ids)
}

/// The module's rules, applied to what was gathered.
///
/// Split out from the queries so they are testable without a database, and so
/// the resolver reads as "gather the grants" and this as "apply them".
struct Rules<'a> {
    organization: Option<&'a OrganizationStanding>,
    kind: &'a str,
    roles: &'a RoleSet,
}

impl Rules<'_> {
    /// Whether the organization-wide grants apply on the organization's records.
    ///
    /// They do for a full member and for an administrator, because the record
    /// is the organization's own surface. A guest's do not: a guest's
    /// organization standing manages and reaches nothing.
    fn cascades(&self) -> bool {
        self.organization.is_some_and(|organization| {
            organization.access == OrganizationAccess::Full
                || organization.granted.iter().any(Role::is_administrator)
        })
    }

    /// Whether organization standing alone reaches every record of the kind.
    fn organization_reaches(&self) -> bool {
        let Some(organization) = self.organization.filter(|_held| self.cascades()) else {
            return false;
        };
        let reads = Permission::new(self.kind, Action::Read);
        organization
            .granted
            .iter()
            .any(|role| role.is_administrator() || role.permissions(self.roles).contains(&reads))
    }

    /// The organization-wide roles that rank on the organization's records.
    fn ranking(&self) -> Vec<Role> {
        match self.organization.filter(|_held| self.cascades()) {
            Some(organization) => organization.granted.clone(),
            None => Vec::new(),
        }
    }

    /// The roles that apply on one record, split by whether they rank there.
    ///
    /// `narrower` holds the team-wide roles of every team of the caller's that
    /// was granted the record; with none and no organization reach, the
    /// caller does not reach it at all. The default role is the caller's to
    /// add to the ranking half.
    fn on_record(&self, narrower: Vec<Vec<Role>>) -> Option<(Vec<Role>, Vec<Role>)> {
        if narrower.is_empty() && !self.organization_reaches() {
            return None;
        }
        Some((self.ranking(), narrower.into_iter().flatten().collect()))
    }

    /// Which records of the kind the caller may `action`.
    ///
    /// Permission is monotone in the roles held, so asking each path on its
    /// own is exact: a record two teams share is actionable when either of
    /// their standings, with the organization's, allows the action there.
    fn scope(&self, teams: Vec<TeamStanding>, default_role: Role, action: Action) -> ResourceScope {
        let mut ranking = self.ranking();
        ranking.push(default_role);

        if self.organization_reaches()
            && Access::ranked(ranking.clone(), Vec::new(), self.roles).can(action, self.kind)
        {
            return ResourceScope::Every;
        }

        let mut scope = ResourceScope::Only(BTreeSet::new());
        for team in teams {
            let access = Access::ranked(ranking.clone(), team.roles, self.roles);
            if access.can(action, self.kind) {
                scope.extend(&team.coverage);
            }
        }
        scope
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use uuid::Uuid;

    use super::{OrganizationStanding, ResourceScope, Rules, TeamStanding, coverage};
    use crate::roles::{Action, RoleSet};
    use crate::tenancy::model::OrganizationAccess;
    use crate::tenancy::role::Role;

    const ROLES: &str = "
roles:
  default:
    models:
      Machine: [read]
  viewer:
    includes: [default]
    models:
      Machine: [read]
  operator:
    includes: [default]
    models:
      Machine: [manage]
  # Includes nothing, so nothing it carries reads machines: a role including
  # `default` here would read them through it.
  bookkeeper:
    models: {}
  admin:
    includes: [operator]
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

    fn organization(access: OrganizationAccess, granted: Vec<Role>) -> OrganizationStanding {
        OrganizationStanding { access, granted }
    }

    fn policy<'a>(organization: Option<&'a OrganizationStanding>, roles: &'a RoleSet) -> Rules<'a> {
        Rules {
            organization,
            kind: "Machine",
            roles,
        }
    }

    fn compiled() -> RoleSet {
        RoleSet::from_yaml(ROLES).expect("roles must parse")
    }

    #[test]
    fn the_default_role_alone_reaches_no_record() {
        // The default role here reads machines, and that is the point: it is
        // everybody's, so it applies where they reach and never makes reach.
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, Vec::new());
        assert!(
            policy(Some(&member), &roles)
                .on_record(Vec::new())
                .is_none()
        );
    }

    #[test]
    fn an_organization_grant_that_reads_the_kind_reaches_every_record() {
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, vec![role("viewer", 1)]);
        let (ranking, narrower) = policy(Some(&member), &roles)
            .on_record(Vec::new())
            .expect("an organization-wide reader reaches the record");

        assert_eq!(ranking.len(), 1);
        assert!(narrower.is_empty());
    }

    #[test]
    fn an_organization_grant_that_does_not_read_the_kind_reaches_nothing() {
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, vec![role("bookkeeper", 1)]);
        assert!(
            policy(Some(&member), &roles)
                .on_record(Vec::new())
                .is_none()
        );
    }

    #[test]
    fn an_administrator_reaches_every_record_even_marked_as_a_guest() {
        let roles = compiled();
        let administrator = organization(OrganizationAccess::Guest, vec![role("admin", 4)]);
        assert!(
            policy(Some(&administrator), &roles)
                .on_record(Vec::new())
                .is_some()
        );
    }

    #[test]
    fn a_guest_reaches_only_what_a_team_of_theirs_was_granted() {
        let roles = compiled();
        let guest = organization(OrganizationAccess::Guest, vec![role("viewer", 1)]);
        let guest_policy = policy(Some(&guest), &roles);

        assert!(
            guest_policy.on_record(Vec::new()).is_none(),
            "a guest's organization grants reach nothing",
        );
        let (ranking, narrower) = guest_policy
            .on_record(vec![vec![role("operator", 2)]])
            .expect("a team grant admits a guest");
        assert!(
            ranking.is_empty(),
            "a guest's organization roles never apply"
        );
        assert_eq!(narrower.len(), 1);
    }

    #[test]
    fn a_team_grant_adds_permissions_and_never_rank() {
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, Vec::new());
        let (mut ranking, narrower) = policy(Some(&member), &roles)
            .on_record(vec![vec![role("admin", 4)]])
            .expect("the team's grant reaches the record");
        ranking.push(role("default", 0));
        let access = super::Access::ranked(ranking, narrower, &roles);

        assert!(access.can(Action::Destroy, "Machine"));
        assert_eq!(
            access.rank(),
            0,
            "a team's administrator ranks nothing here"
        );
        assert!(!access.is_administrator());
    }

    #[test]
    fn a_team_only_member_reaches_what_the_team_was_granted() {
        let roles = compiled();
        let (ranking, narrower) = policy(None, &roles)
            .on_record(vec![Vec::new()])
            .expect("belonging to a granted team admits");
        assert!(ranking.is_empty() && narrower.is_empty());
        assert!(policy(None, &roles).on_record(Vec::new()).is_none());
    }

    #[test]
    fn a_scope_takes_in_what_each_path_allows() {
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, Vec::new());
        let shared = Uuid::new_v4();
        let other = Uuid::new_v4();
        let teams = vec![
            TeamStanding {
                roles: vec![role("operator", 2)],
                coverage: ResourceScope::Only(BTreeSet::from([shared])),
            },
            TeamStanding {
                roles: Vec::new(),
                coverage: ResourceScope::Only(BTreeSet::from([other])),
            },
        ];

        let scope = policy(Some(&member), &roles).scope(teams, role("default", 0), Action::Update);
        assert_eq!(
            scope,
            ResourceScope::Only(BTreeSet::from([shared])),
            "only the team whose roles update machines widens the update scope",
        );
    }

    #[test]
    fn a_scope_is_every_record_for_an_organization_reader() {
        let roles = compiled();
        let member = organization(OrganizationAccess::Full, vec![role("viewer", 1)]);
        let scope =
            policy(Some(&member), &roles).scope(Vec::new(), role("default", 0), Action::Read);
        assert_eq!(scope, ResourceScope::Every);
    }

    #[test]
    fn one_grant_naming_no_record_covers_every_record() {
        let one = Uuid::new_v4();
        assert_eq!(coverage(vec![Some(one), None]), ResourceScope::Every);
        assert_eq!(
            coverage(vec![Some(one)]),
            ResourceScope::Only(BTreeSet::from([one])),
        );
        assert!(!coverage(Vec::new()).includes(one));
    }
}
