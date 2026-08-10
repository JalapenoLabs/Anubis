//! The framework-owned tenancy models.

use chrono::{DateTime, Utc};
use diesel::prelude::{Insertable, Queryable, Selectable};
use serde::Serialize;
use uuid::Uuid;

use crate::schema::{organization_memberships, organizations, team_memberships, teams};

/// The top-level tenant: owns teams, billing, and org-wide settings.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = organizations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Organization {
    /// Primary key.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// When the organization was created.
    pub created_at: DateTime<Utc>,
    /// When the organization was last updated.
    pub updated_at: DateTime<Utc>,
}

/// The working tenant: all domain resources chain ownership back to a team.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = teams)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Team {
    /// Primary key.
    pub id: Uuid,
    /// The organization this team belongs to.
    pub organization_id: Uuid,
    /// Display name.
    pub name: String,
    /// When the team was created.
    pub created_at: DateTime<Utc>,
    /// When the team was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Joins a user to an organization with org-level roles.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = organization_memberships)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct OrganizationMembership {
    /// Primary key.
    pub id: Uuid,
    /// The organization joined.
    pub organization_id: Uuid,
    /// The member.
    pub user_id: Uuid,
    /// Role keys granted at the organization level.
    pub roles: Vec<String>,
    /// When the membership was created.
    pub created_at: DateTime<Utc>,
    /// When the membership was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Joins a user to a team with team-level roles.
///
/// Domain resources are assigned to team memberships, never directly to
/// users; `user_id` stays null for invited people until they claim it.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = team_memberships)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct TeamMembership {
    /// Primary key.
    pub id: Uuid,
    /// The team joined.
    pub team_id: Uuid,
    /// The member, once the membership is claimed.
    pub user_id: Option<Uuid>,
    /// Role keys granted at the team level.
    pub roles: Vec<String>,
    /// When the membership was created.
    pub created_at: DateTime<Utc>,
    /// When the membership was last updated.
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = organizations)]
pub(crate) struct NewOrganization<'a> {
    pub name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = teams)]
pub(crate) struct NewTeam<'a> {
    pub organization_id: Uuid,
    pub name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = organization_memberships)]
pub(crate) struct NewOrganizationMembership<'a> {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub roles: &'a [String],
}

#[derive(Insertable)]
#[diesel(table_name = team_memberships)]
pub(crate) struct NewTeamMembership<'a> {
    pub team_id: Uuid,
    pub user_id: Option<Uuid>,
    pub roles: &'a [String],
}
