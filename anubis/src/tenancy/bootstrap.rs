//! Personal-organization bootstrap at signup.

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

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

/// Name of the team every new organization starts with.
const DEFAULT_TEAM_NAME: &str = "General";

/// Creates the personal organization, default team, and admin memberships.
///
/// Every user gets this at signup so solo use has zero tenancy ceremony: the
/// organization is named after the email's local part and holds one team. The
/// caller runs this inside the same transaction that inserts the user, so a
/// half-bootstrapped account can never exist.
pub(crate) async fn create_personal_organization(
    connection: &mut AsyncPgConnection,
    user: &User,
) -> Result<(Organization, Team), diesel::result::Error> {
    // Registration validated the email, so the local part is non-empty.
    let organization_name = user.email.split('@').next().unwrap_or("Personal");
    let admin_roles = vec![ADMIN_ROLE.to_owned()];

    let organization: Organization = diesel::insert_into(organizations::table)
        .values(NewOrganization {
            name: organization_name,
        })
        .returning(Organization::as_returning())
        .get_result(connection)
        .await?;

    let team: Team = diesel::insert_into(teams::table)
        .values(NewTeam {
            organization_id: organization.id,
            name: DEFAULT_TEAM_NAME,
        })
        .returning(Team::as_returning())
        .get_result(connection)
        .await?;

    diesel::insert_into(organization_memberships::table)
        .values(NewOrganizationMembership {
            organization_id: organization.id,
            user_id: user.id,
            roles: &admin_roles,
        })
        .execute(connection)
        .await?;

    diesel::insert_into(team_memberships::table)
        .values(NewTeamMembership {
            team_id: team.id,
            user_id: Some(user.id),
            roles: &admin_roles,
        })
        .execute(connection)
        .await?;

    Ok((organization, team))
}
