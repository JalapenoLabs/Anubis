//! Invitation model and database operations.
//!
//! Team invitations pre-create an unclaimed [`TeamMembership`], so resources
//! can be assigned to the invitee before they join, and the membership (with
//! its assignments) survives the claim. Organization invitations create the
//! organization membership at claim time. The emailed token follows the
//! framework's token discipline: random 256 bits to the recipient, SHA-256 at
//! rest, single use.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::token;
use crate::schema::{invitations, organization_memberships, team_memberships};
use crate::tenancy::model::{NewOrganizationMembership, NewTeamMembership, TeamMembership};

/// How long an invitation stays claimable.
pub const INVITATION_TTL_DAYS: i64 = 14;

/// A pending invitation to a team or an organization.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invitations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Invitation {
    /// Primary key.
    pub id: Uuid,
    /// Normalized email address the invitation was sent to.
    pub email: String,
    /// The organization the invitation belongs to.
    pub organization_id: Uuid,
    /// The team joined on claim; null for organization-level invitations.
    pub team_id: Option<Uuid>,
    /// The unclaimed membership created alongside a team invitation.
    pub team_membership_id: Option<Uuid>,
    /// Role keys granted on claim.
    pub roles: Vec<String>,
    /// Who sent the invitation, if they still exist.
    pub invited_by: Option<Uuid>,
    /// When the invitation was created.
    pub created_at: DateTime<Utc>,
    /// When the invitation stops being claimable.
    pub expires_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = invitations)]
struct NewInvitation<'a> {
    email: &'a str,
    organization_id: Uuid,
    team_id: Option<Uuid>,
    team_membership_id: Option<Uuid>,
    roles: &'a [String],
    invited_by: Uuid,
    token_hash: &'a str,
    expires_at: DateTime<Utc>,
}

/// What an invitation targets.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InvitationTarget {
    Organization(Uuid),
    Team {
        team_id: Uuid,
        organization_id: Uuid,
    },
}

/// Creates an invitation, returning it and the raw token for the email link.
///
/// The row comes back rather than being read again afterwards, so the caller's
/// response, its audit record, and what was stored are the same three facts
/// from one write.
///
/// Replaces any pending invitation for the same email and target (including
/// its unclaimed membership), so re-inviting is always safe. Team invitations
/// pre-create the unclaimed team membership.
pub(crate) async fn create(
    connection: &mut AsyncPgConnection,
    target: InvitationTarget,
    email: &str,
    roles: &[String],
    invited_by: Uuid,
) -> Result<(Invitation, String), diesel::result::Error> {
    // Replace any pending invitation for this email and target. Deleting the
    // invitation cascades nothing; the linked unclaimed membership is removed
    // explicitly.
    let (organization_id, team_id) = match target {
        InvitationTarget::Organization(organization_id) => (organization_id, None),
        InvitationTarget::Team {
            team_id,
            organization_id,
        } => (organization_id, Some(team_id)),
    };

    let stale_membership_ids: Vec<Option<Uuid>> = diesel::delete(
        invitations::table
            .filter(invitations::email.eq(email))
            .filter(invitations::organization_id.eq(organization_id))
            .filter(invitations::team_id.is_not_distinct_from(team_id)),
    )
    .returning(invitations::team_membership_id)
    .get_results(connection)
    .await?;

    let stale_ids: Vec<Uuid> = stale_membership_ids.into_iter().flatten().collect();
    if !stale_ids.is_empty() {
        diesel::delete(
            team_memberships::table
                .filter(team_memberships::id.eq_any(stale_ids))
                .filter(team_memberships::user_id.is_null()),
        )
        .execute(connection)
        .await?;
    }

    let team_membership_id = match team_id {
        None => None,
        Some(team_id) => {
            let membership: TeamMembership = diesel::insert_into(team_memberships::table)
                .values(NewTeamMembership {
                    team_id,
                    user_id: None,
                    roles,
                })
                .returning(TeamMembership::as_returning())
                .get_result(connection)
                .await?;
            Some(membership.id)
        }
    };

    let raw_token = token::generate();
    let token_hash = token::hash(&raw_token);

    let invitation: Invitation = diesel::insert_into(invitations::table)
        .values(NewInvitation {
            email,
            organization_id,
            team_id,
            team_membership_id,
            roles,
            invited_by,
            token_hash: &token_hash,
            expires_at: Utc::now() + Duration::days(INVITATION_TTL_DAYS),
        })
        .returning(Invitation::as_returning())
        .get_result(connection)
        .await?;

    Ok((invitation, raw_token))
}

/// Consumes an invitation token, joining the claimant to its target.
///
/// Returns the claimed invitation, or `None` for unknown or expired tokens.
/// Team claims adopt the pre-created membership (keeping its id, roles, and
/// any resource assignments); organization claims insert the membership. A
/// claimant who is already a member simply absorbs the invitation.
pub(crate) async fn claim(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
    claimant: Uuid,
) -> Result<Option<Invitation>, diesel::result::Error> {
    let token_hash = token::hash(raw_token);

    let invitation: Option<Invitation> = diesel::delete(
        invitations::table
            .filter(invitations::token_hash.eq(&token_hash))
            .filter(invitations::expires_at.gt(Utc::now())),
    )
    .returning(Invitation::as_returning())
    .get_result(connection)
    .await
    .optional()?;

    let Some(invitation) = invitation else {
        return Ok(None);
    };

    match invitation.team_membership_id {
        Some(membership_id) => {
            let adopted = diesel::update(
                team_memberships::table
                    .filter(team_memberships::id.eq(membership_id))
                    .filter(team_memberships::user_id.is_null()),
            )
            .set((team_memberships::user_id.eq(claimant),))
            .execute(connection)
            .await;

            match adopted {
                Ok(_count) => {}
                // The claimant already belongs to the team; drop the
                // now-redundant unclaimed membership.
                Err(diesel::result::Error::DatabaseError(
                    DatabaseErrorKind::UniqueViolation,
                    _details,
                )) => {
                    diesel::delete(
                        team_memberships::table
                            .filter(team_memberships::id.eq(membership_id))
                            .filter(team_memberships::user_id.is_null()),
                    )
                    .execute(connection)
                    .await?;
                }
                Err(other) => return Err(other),
            }
        }
        None => {
            diesel::insert_into(organization_memberships::table)
                .values(NewOrganizationMembership {
                    organization_id: invitation.organization_id,
                    user_id: claimant,
                    roles: &invitation.roles,
                })
                .on_conflict_do_nothing()
                .execute(connection)
                .await?;
        }
    }

    Ok(Some(invitation))
}
