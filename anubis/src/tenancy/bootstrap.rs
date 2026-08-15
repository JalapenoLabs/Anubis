//! Creating organizations and teams, at signup and on demand.
//!
//! [`bootstrap_account`] is the one function both signup paths call, email
//! registration and OAuth first sign-in alike, so what an account joins cannot
//! drift between the two. What it does is a deployment decision, read once
//! from `ANUBIS_BOOTSTRAP` and `ANUBIS_SHARED_ORGANIZATION` when configuration
//! loads (see [`crate::config`]) and carried on
//! [`crate::config::AppConfig::bootstrap`]:
//!
//! | Mode | What a new account joins |
//! |---|---|
//! | `personal` | An organization of its own, named after the address's local part, holding one team. The default, and what an unset variable means |
//! | `shared` | The one organization `ANUBIS_SHARED_ORGANIZATION` names, and its default team |
//!
//! `personal` is public multi-tenant SaaS, where two accounts are two tenants
//! until somebody invites somebody. `shared` is internal tooling, where every
//! account is a colleague and the whole point is that they all see the same
//! resources.
//!
//! The shared organization is created by the first signup that finds it
//! missing, inside that signup's own transaction, so the account and the
//! tenancy it lands in commit together or not at all. Concurrent first signups
//! are ordered by [`SHARED_BOOTSTRAP_LOCK_KEY`], so a deployment ends with the
//! one organization it named rather than one per racer.
//!
//! Whoever creates an organization administers it, which is true of every
//! organization in the system and is what keeps a shared deployment
//! administrable from its first signup. Every account joining an organization
//! that already exists gets [`DEFAULT_ROLE`], and promoting one is an
//! administrator's act.
//!
//! The management endpoints create further organizations and teams through the
//! same functions, so every organization in the system looks alike and every
//! creator ends up an admin of what they created.

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::User;
use crate::schema::{organization_memberships, organizations, team_memberships, teams};
use crate::tenancy::model::{
    NewOrganization, NewOrganizationMembership, NewTeam, NewTeamMembership, Organization, Team,
};

/// Role key granted to the creator of an organization or team.
///
/// The roles.yml compiler formalizes role definitions; until then this is the
/// one well-known key.
pub const ADMIN_ROLE: &str = "admin";

/// Role key granted when a caller asks for no roles at all.
///
/// The starter's `config/roles.yml` ships it as the read-only baseline; an
/// application that removes the key makes role-less requests fail validation,
/// which is the honest outcome.
pub(crate) const DEFAULT_ROLE: &str = "default";

/// Longest accepted organization or team name.
pub(crate) const MAX_NAME_CHARS: usize = 100;

/// Name of the team every new organization starts with.
const DEFAULT_TEAM_NAME: &str = "General";

/// The advisory lock a shared bootstrap creates its organization under.
///
/// Two first signups arriving together would each find no organization and
/// each create one, leaving a deployment with two organizations of the same
/// name and no way to say which is the one everybody joins. A
/// transaction-scoped lock orders them: the winner creates and commits, and
/// the loser wakes holding the lock, finds what the winner committed, and
/// joins that. Transaction scope is what makes the second half true, since the
/// lock is held until the creating transaction ends and is released by a
/// rollback exactly as surely as by a commit.
///
/// The value is arbitrary but must never change: it is the name concurrent
/// transactions agree on, and a different value is a different lock. It
/// follows the migration lock documented at [`crate::db`] in the framework's
/// own series, the ASCII bytes of `ANUBIS` followed by a counter.
const SHARED_BOOTSTRAP_LOCK_KEY: i64 = 0x414E_5542_4953_0002;

/// What a new account joins at signup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum BootstrapMode {
    /// An organization and default team of the account's own. The default.
    #[default]
    Personal,
    /// The one organization of this name, and its default team.
    ///
    /// Never empty: the name is what the organization is found by, so
    /// configuration refuses to start without one rather than putting every
    /// account somewhere unnamed.
    Shared(String),
}

/// Returns `true` when the held role keys include [`ADMIN_ROLE`].
pub(crate) fn holds_admin(roles: &[String]) -> bool {
    roles.iter().any(|role| role == ADMIN_ROLE)
}

/// Creates or joins the tenancy a new account starts in.
///
/// The one function both signup paths call, `POST /auth/register` and the
/// OAuth callback that meets a verified address no account owns yet, so email
/// and OAuth signup cannot drift. The caller runs it inside the same
/// transaction that inserts the user, so a half-bootstrapped account can never
/// exist.
///
/// Returns the organization and the team the account landed in.
pub(crate) async fn bootstrap_account(
    connection: &mut AsyncPgConnection,
    mode: &BootstrapMode,
    user: &User,
) -> Result<(Organization, Team), diesel::result::Error> {
    match mode {
        BootstrapMode::Personal => create_personal_organization(connection, user).await,
        BootstrapMode::Shared(name) => join_shared_organization(connection, user.id, name).await,
    }
}

/// Creates the personal organization, default team, and admin memberships.
///
/// The organization is named after the email's local part and holds one team,
/// so solo use has zero tenancy ceremony.
async fn create_personal_organization(
    connection: &mut AsyncPgConnection,
    user: &User,
) -> Result<(Organization, Team), diesel::result::Error> {
    // Registration validated the email, so the local part is non-empty.
    let name = user.email.split('@').next().unwrap_or("Personal");
    create_organization(connection, user.id, name).await
}

/// Puts the account in the deployment's one shared organization.
///
/// The first signup creates it and administers it; every signup after that
/// joins it with [`DEFAULT_ROLE`], in the organization and in its default
/// team.
async fn join_shared_organization(
    connection: &mut AsyncPgConnection,
    user: Uuid,
    name: &str,
) -> Result<(Organization, Team), diesel::result::Error> {
    // The steady state, and the reason the lock below is not taken here: after
    // the first signup there is an organization to find and nothing to order.
    if let Some(organization) = find_shared_organization(connection, name).await? {
        return join_organization(connection, organization, user).await;
    }

    take_shared_bootstrap_lock(connection).await?;

    // Whoever queued behind the creator finds what the creator committed,
    // because the lock is only released by that commit.
    if let Some(organization) = find_shared_organization(connection, name).await? {
        return join_organization(connection, organization, user).await;
    }

    create_organization(connection, user, name).await
}

/// Finds the shared organization by the name configuration gave it.
///
/// The oldest wins, so an organization somebody later creates under the same
/// name never becomes the one new accounts land in.
async fn find_shared_organization(
    connection: &mut AsyncPgConnection,
    name: &str,
) -> Result<Option<Organization>, diesel::result::Error> {
    organizations::table
        .filter(organizations::name.eq(name))
        .order((organizations::created_at.asc(), organizations::id.asc()))
        .select(Organization::as_select())
        .first(connection)
        .await
        .optional()
}

/// Takes [`SHARED_BOOTSTRAP_LOCK_KEY`] for the rest of the caller's transaction.
///
/// The key is a compile-time integer constant, so rendering it into the
/// statement carries no input to inject.
async fn take_shared_bootstrap_lock(
    connection: &mut AsyncPgConnection,
) -> Result<(), diesel::result::Error> {
    diesel::sql_query(format!(
        "SELECT pg_advisory_xact_lock({SHARED_BOOTSTRAP_LOCK_KEY})"
    ))
    .execute(connection)
    .await
    .map(|_rows| ())
}

/// Joins an account to an organization it did not create, and to its team.
async fn join_organization(
    connection: &mut AsyncPgConnection,
    organization: Organization,
    user: Uuid,
) -> Result<(Organization, Team), diesel::result::Error> {
    let default_roles = vec![DEFAULT_ROLE.to_owned()];

    diesel::insert_into(organization_memberships::table)
        .values(NewOrganizationMembership {
            organization_id: organization.id,
            user_id: user,
            roles: &default_roles,
        })
        .execute(connection)
        .await?;

    // The default team is the one the organization was created with, which is
    // its oldest; renaming it does not move it elsewhere.
    let default_team: Option<Team> = teams::table
        .filter(teams::organization_id.eq(organization.id))
        .order((teams::created_at.asc(), teams::id.asc()))
        .select(Team::as_select())
        .first(connection)
        .await
        .optional()?;

    let team = match default_team {
        Some(team) => {
            diesel::insert_into(team_memberships::table)
                .values(NewTeamMembership {
                    team_id: team.id,
                    user_id: Some(user),
                    roles: &default_roles,
                })
                .execute(connection)
                .await?;
            team
        }
        // An organization whose teams were all dissolved still owes this
        // account somewhere to land, so it gets its default team back, under
        // the rule that holds everywhere else: whoever creates a team
        // administers it.
        None => create_team(connection, organization.id, DEFAULT_TEAM_NAME, user).await?,
    };

    Ok((organization, team))
}

/// Creates an organization with its default team, both administered by `owner`.
///
/// The personal organization at signup and an organization created later are
/// the same thing; nothing marks one as special. Run it in a transaction so an
/// organization without its team, or without its admin, can never exist.
pub(crate) async fn create_organization(
    connection: &mut AsyncPgConnection,
    owner: Uuid,
    name: &str,
) -> Result<(Organization, Team), diesel::result::Error> {
    let admin_roles = vec![ADMIN_ROLE.to_owned()];

    let organization: Organization = diesel::insert_into(organizations::table)
        .values(NewOrganization { name })
        .returning(Organization::as_returning())
        .get_result(connection)
        .await?;

    diesel::insert_into(organization_memberships::table)
        .values(NewOrganizationMembership {
            organization_id: organization.id,
            user_id: owner,
            roles: &admin_roles,
        })
        .execute(connection)
        .await?;

    let team = create_team(connection, organization.id, DEFAULT_TEAM_NAME, owner).await?;

    Ok((organization, team))
}

/// Creates a team in an organization, with `owner` as its admin member.
pub(crate) async fn create_team(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    name: &str,
    owner: Uuid,
) -> Result<Team, diesel::result::Error> {
    let admin_roles = vec![ADMIN_ROLE.to_owned()];

    let team: Team = diesel::insert_into(teams::table)
        .values(NewTeam {
            organization_id,
            name,
        })
        .returning(Team::as_returning())
        .get_result(connection)
        .await?;

    diesel::insert_into(team_memberships::table)
        .values(NewTeamMembership {
            team_id: team.id,
            user_id: Some(owner),
            roles: &admin_roles,
        })
        .execute(connection)
        .await?;

    Ok(team)
}

#[cfg(test)]
mod tests {
    use super::BootstrapMode;

    #[test]
    fn a_deployment_that_configures_nothing_gives_every_account_its_own_tenancy() {
        assert_eq!(BootstrapMode::default(), BootstrapMode::Personal);
    }
}
