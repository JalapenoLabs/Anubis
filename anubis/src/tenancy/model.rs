//! The framework-owned tenancy models.

use std::collections::HashMap;
use std::io::Write;

use chrono::{DateTime, Utc};
use diesel::deserialize::{self, FromSql, FromSqlRow};
use diesel::expression::AsExpression;
use diesel::pg::{Pg, PgValue};
use diesel::prelude::*;
use diesel::serialize::{self, Output, ToSql};
use diesel::sql_types::Text;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::http::FieldOption;
use crate::schema::{
    invitations, organization_memberships, organizations, sub_tenant_memberships, sub_tenants,
    team_memberships, teams, users,
};

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

/// The tier between an organization and its teams: it owns the work.
///
/// A sub-tenant is what GCP calls a project, Jira calls a project, and GitHub
/// calls a repository. It holds its own membership and its own teams, while
/// the organization above it stays the billing and policy umbrella. Every
/// organization has at least one, created with it, so an application that
/// never surfaces the tier still has a complete ownership chain.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = sub_tenants)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SubTenant {
    /// Primary key.
    pub id: Uuid,
    /// The organization this sub-tenant belongs to.
    pub organization_id: Uuid,
    /// Display name.
    pub name: String,
    /// When the sub-tenant was created.
    pub created_at: DateTime<Utc>,
    /// When the sub-tenant was last updated.
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
    /// The sub-tenant this team is scoped to, or `None` for an
    /// organization-level team that every sub-tenant inherits.
    pub sub_tenant_id: Option<Uuid>,
}

/// How far an organization membership reaches into the organization's work.
///
/// The distinction is GitHub's between an organization member and an outside
/// collaborator: a full member's standing cascades into every sub-tenant, and
/// a guest reaches only the sub-tenants and teams they were granted by name.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsExpression, FromSqlRow, Default,
)]
#[diesel(sql_type = Text)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationAccess {
    /// Reaches every sub-tenant in the organization.
    #[default]
    Full,
    /// Reaches only what was granted explicitly.
    Guest,
}

impl OrganizationAccess {
    /// The value's spelling in the database and in JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Guest => "guest",
        }
    }
}

impl ToSql<Text, Pg> for OrganizationAccess {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        out.write_all(self.as_str().as_bytes())?;
        Ok(serialize::IsNull::No)
    }
}

impl FromSql<Text, Pg> for OrganizationAccess {
    fn from_sql(bytes: PgValue<'_>) -> deserialize::Result<Self> {
        // A `CHECK` constraint keeps any other value out of the column, so an
        // unknown one is a corrupted row rather than an access level to guess
        // at. Failing loudly beats quietly reading it as the weaker of the two.
        match bytes.as_bytes() {
            b"full" => Ok(Self::Full),
            b"guest" => Ok(Self::Guest),
            other => Err(format!(
                "unknown organization access {:?}",
                String::from_utf8_lossy(other)
            )
            .into()),
        }
    }
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
    /// How far the membership reaches into the organization's sub-tenants.
    pub access: OrganizationAccess,
    /// When the member was suspended, or `None` while they are in good
    /// standing. A suspension cuts them out of the organization entirely.
    pub suspended_at: Option<DateTime<Utc>>,
}

/// Joins a user to a sub-tenant with sub-tenant-level roles.
///
/// This is the explicit grant a guest reaches a sub-tenant through, and the
/// row an administrator suspends to cut somebody out of one sub-tenant without
/// touching their standing anywhere else.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = sub_tenant_memberships)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SubTenantMembership {
    /// Primary key.
    pub id: Uuid,
    /// The sub-tenant joined.
    pub sub_tenant_id: Uuid,
    /// The member.
    pub user_id: Uuid,
    /// Role keys granted at the sub-tenant level.
    pub roles: Vec<String>,
    /// When the member was suspended from this sub-tenant, or `None` while
    /// they are in good standing.
    pub suspended_at: Option<DateTime<Utc>>,
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

impl TeamMembership {
    /// The user's membership in a team, or `None` when they are not a member.
    ///
    /// This is the last link of every scaffolded model's ownership chain:
    /// resolve the record's `team_id`, then ask this whether the caller
    /// belongs there. Application tables cannot join framework tables in one
    /// Diesel query (Rust's orphan rules forbid the cross-crate trait
    /// implementations), so the chain ends in this indexed lookup.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn for_user(
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        team_id: Uuid,
    ) -> QueryResult<Option<Self>> {
        team_memberships::table
            .filter(team_memberships::team_id.eq(team_id))
            .filter(team_memberships::user_id.eq(user_id))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()
    }

    /// The team's memberships, as the options an assignment field offers.
    ///
    /// This is the framework half of Bullet Train's signature `belongs_to`:
    /// `lead_id:super_select{class_name=TeamMembership}` assigns a record to a
    /// person on the team, and a person who was invited but has not signed up
    /// yet is one of them. Memberships live in a framework table, so an
    /// application crate cannot reach them through its own Diesel query graph
    /// (Rust's orphan rules forbid the cross-crate trait implementations); a
    /// generated `valid_*` method calls this instead.
    ///
    /// One definition, two duties, exactly as `docs/scaffolding.md` requires:
    /// it fills the association's options endpoint and validates every id
    /// submitted through a form, so a request can never assign a record to
    /// another team's member. Rows are ordered by the label a person reads.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn valid_for_team(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
    ) -> QueryResult<Vec<FieldOption>> {
        let rows: Vec<RosterRow> = team_memberships::table
            .left_join(users::table)
            .left_join(
                invitations::table
                    .on(invitations::team_membership_id.eq(team_memberships::id.nullable())),
            )
            .filter(team_memberships::team_id.eq(team_id))
            .select((
                team_memberships::id,
                users::first_name.nullable(),
                users::last_name.nullable(),
                users::email.nullable(),
                invitations::email.nullable(),
            ))
            .load(connection)
            .await?;

        let mut options: Vec<FieldOption> = rows
            .into_iter()
            .map(|row| FieldOption {
                value: row.0,
                label: member_label(&row),
            })
            .collect();
        options.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then(left.value.cmp(&right.value))
        });
        Ok(options)
    }

    /// The labels of the memberships `membership_ids` names, keyed by id.
    ///
    /// One query serves a whole page, which is what keeps a list endpoint that
    /// serializes an assignment from turning into a query per row. Ids that
    /// name no membership are simply absent from the map.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn labels_for(
        connection: &mut AsyncPgConnection,
        membership_ids: &[Uuid],
    ) -> QueryResult<HashMap<Uuid, String>> {
        if membership_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let rows: Vec<RosterRow> = team_memberships::table
            .left_join(users::table)
            .left_join(
                invitations::table
                    .on(invitations::team_membership_id.eq(team_memberships::id.nullable())),
            )
            .filter(team_memberships::id.eq_any(membership_ids))
            .select((
                team_memberships::id,
                users::first_name.nullable(),
                users::last_name.nullable(),
                users::email.nullable(),
                invitations::email.nullable(),
            ))
            .load(connection)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.0, member_label(&row)))
            .collect())
    }
}

/// One roster row: the membership id, the account's name and email, and the
/// email a pending invitation was sent to.
type RosterRow = (
    Uuid,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// How a person recognizes a membership: their name, their email, or the
/// address their invitation went to.
fn member_label(row: &RosterRow) -> String {
    let (_id, first_name, last_name, email, invited_email) = row;

    let name = [first_name.as_deref(), last_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !name.is_empty() {
        return name;
    }
    if let Some(email) = email.as_deref().or(invited_email.as_deref()) {
        return email.to_owned();
    }
    // A membership with neither an account nor a live invitation is a person
    // whose invitation was revoked mid-flight; naming it is better than
    // dropping it out of a list a record may still point at.
    "Invited member".to_owned()
}

#[derive(Insertable)]
#[diesel(table_name = organizations)]
pub(crate) struct NewOrganization<'a> {
    pub name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = sub_tenants)]
pub(crate) struct NewSubTenant<'a> {
    pub organization_id: Uuid,
    pub name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = teams)]
pub(crate) struct NewTeam<'a> {
    pub organization_id: Uuid,
    pub name: &'a str,
    pub sub_tenant_id: Option<Uuid>,
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
