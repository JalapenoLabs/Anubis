//! Invitation model and database operations.
//!
//! Team invitations pre-create an unclaimed [`TeamMembership`], so resources
//! can be assigned to the invitee before they join, and the roles the
//! invitation offers are granted on that membership straight away: the claim
//! hands over the membership with its id, its grants, and any resource
//! assignments intact. Organization invitations create the organization
//! membership at claim time, so the roles they offer wait in
//! `invitation_roles` until then. The emailed token follows the framework's
//! token discipline: random 256 bits to the recipient, SHA-256 at rest, single
//! use.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::token;
use crate::schema::{
    invitation_roles, invitations, organization_memberships, organizations, role_grants,
    team_memberships,
};
use crate::tenancy::grants::{self, Holder};
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
    invited_by: Uuid,
    token_hash: &'a str,
    expires_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = invitation_roles)]
#[expect(
    clippy::struct_field_names,
    reason = "a join row is nothing but the ids it joins, named as their columns"
)]
struct NewInvitationRole {
    invitation_id: Uuid,
    role_id: Uuid,
    organization_id: Uuid,
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

/// A claimed invitation, and the roles the claim granted.
#[derive(Debug, Clone)]
pub(crate) struct Claimed {
    /// The invitation, which no longer exists.
    pub invitation: Invitation,
    /// The roles the claimant now holds through it: empty when they already
    /// belonged, because a claim by a member is absorbed and changes nothing.
    pub role_ids: Vec<Uuid>,
}

/// Creates an invitation, returning it and the raw token for the email link.
///
/// The row comes back rather than being read again afterwards, so the caller's
/// response, its audit record, and what was stored are the same three facts
/// from one write. `role_ids` are roles of the target's organization the
/// caller has already been allowed to offer.
///
/// Replaces any pending invitation for the same email and target (including
/// its unclaimed membership and that membership's grants), so re-inviting is
/// always safe.
pub(crate) async fn create(
    connection: &mut AsyncPgConnection,
    target: InvitationTarget,
    email: &str,
    role_ids: &[Uuid],
    invited_by: Uuid,
) -> Result<(Invitation, String), diesel::result::Error> {
    let (organization_id, team_id) = match target {
        InvitationTarget::Organization(organization_id) => (organization_id, None),
        InvitationTarget::Team {
            team_id,
            organization_id,
        } => (organization_id, Some(team_id)),
    };

    // Replace any pending invitation for this email and target. Its offered
    // roles cascade with it; the linked unclaimed membership is removed
    // explicitly, and its grants cascade with that.
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
                })
                .returning(TeamMembership::as_returning())
                .get_result(connection)
                .await?;
            for role_id in role_ids {
                grants::grant(
                    connection,
                    organization_id,
                    *role_id,
                    Holder::Team {
                        team_id,
                        membership_id: membership.id,
                        resource_id: None,
                    },
                    Some(invited_by),
                )
                .await?;
            }
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
            invited_by,
            token_hash: &token_hash,
            expires_at: Utc::now() + Duration::days(INVITATION_TTL_DAYS),
        })
        .returning(Invitation::as_returning())
        .get_result(connection)
        .await?;

    if team_id.is_none() && !role_ids.is_empty() {
        let offered: Vec<NewInvitationRole> = role_ids
            .iter()
            .map(|role_id| NewInvitationRole {
                invitation_id: invitation.id,
                role_id: *role_id,
                organization_id,
            })
            .collect();
        diesel::insert_into(invitation_roles::table)
            .values(&offered)
            .on_conflict_do_nothing()
            .execute(connection)
            .await?;
    }

    Ok((invitation, raw_token))
}

/// The pending invitation [`create`] would replace for `email`, if any.
///
/// Answers its id and the membership a team invitation pre-created.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn pending_for(
    connection: &mut AsyncPgConnection,
    target: InvitationTarget,
    email: &str,
) -> QueryResult<Option<(Uuid, Option<Uuid>)>> {
    let (organization_id, team_id) = match target {
        InvitationTarget::Organization(organization_id) => (organization_id, None),
        InvitationTarget::Team {
            team_id,
            organization_id,
        } => (organization_id, Some(team_id)),
    };

    invitations::table
        .filter(invitations::email.eq(email))
        .filter(invitations::organization_id.eq(organization_id))
        .filter(invitations::team_id.is_not_distinct_from(team_id))
        .select((invitations::id, invitations::team_membership_id))
        .first(connection)
        .await
        .optional()
}

/// The roles a pending invitation offers.
///
/// A team invitation's are the grants waiting on the membership it
/// pre-created; an organization invitation's are the roles it names.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn offered_by(
    connection: &mut AsyncPgConnection,
    invitation_id: Uuid,
    pending_membership: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    match pending_membership {
        Some(membership_id) => {
            role_grants::table
                .filter(role_grants::team_membership_id.eq(membership_id))
                .filter(role_grants::resource_id.is_null())
                .select(role_grants::role_id)
                .load(connection)
                .await
        }
        None => Ok(offered_role_ids(connection, &[invitation_id])
            .await?
            .into_iter()
            .map(|(_invitation_id, role_id)| role_id)
            .collect()),
    }
}

/// The roles an organization invitation offers, keyed by invitation.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn offered_role_ids(
    connection: &mut AsyncPgConnection,
    invitation_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    invitation_roles::table
        .filter(invitation_roles::invitation_id.eq_any(invitation_ids))
        .select((invitation_roles::invitation_id, invitation_roles::role_id))
        .load(connection)
        .await
}

/// Consumes an invitation token, joining the claimant to its target.
///
/// Returns `None` for unknown or expired tokens. Team claims adopt the
/// pre-created membership (keeping its id, grants, and any resource
/// assignments); organization claims insert the membership and grant the roles
/// the invitation still offers, so a role deleted while it was in flight is
/// simply not granted. A claimant who is already a member absorbs the
/// invitation and keeps exactly what they held.
///
/// The organization row is locked first, as every role and grant change locks
/// it, so a role deleted or a grant changed while the claim is in flight lands
/// wholly before or after it, rather than as a foreign key failure or a
/// deadlock between the two. The invitation row is locked next, so two claims
/// of one token are ordered and the second finds nothing.
pub(crate) async fn claim(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
    claimant: Uuid,
) -> Result<Option<Claimed>, diesel::result::Error> {
    let token_hash = token::hash(raw_token);

    let organization_id: Option<Uuid> = invitations::table
        .filter(invitations::token_hash.eq(&token_hash))
        .select(invitations::organization_id)
        .first(connection)
        .await
        .optional()?;
    let Some(organization_id) = organization_id else {
        return Ok(None);
    };
    // An organization deleted since leaves no invitation behind, which the
    // locked read below answers as an unknown token.
    organizations::table
        .find(organization_id)
        .select(organizations::id)
        .for_update()
        .execute(connection)
        .await?;

    let invitation: Option<Invitation> = invitations::table
        .filter(invitations::token_hash.eq(&token_hash))
        .filter(invitations::expires_at.gt(Utc::now()))
        .for_update()
        .select(Invitation::as_select())
        .first(connection)
        .await
        .optional()?;
    let Some(invitation) = invitation else {
        return Ok(None);
    };

    let offered: Vec<Uuid> = offered_role_ids(connection, &[invitation.id])
        .await?
        .into_iter()
        .map(|(_invitation_id, role_id)| role_id)
        .collect();

    // Deleting the invitation takes its offered roles with it, which is why
    // they were read first.
    diesel::delete(invitations::table.find(invitation.id))
        .execute(connection)
        .await?;

    let role_ids = match invitation.team_id {
        Some(team_id) => {
            // A team invitation's membership cascades it away when deleted, so
            // one without the other is a broken row, and the claim rolls back.
            let Some(membership_id) = invitation.team_membership_id else {
                tracing::error!(
                    invitation.id = %invitation.id,
                    "team invitation {{invitation.id}} has no membership to hand over",
                );
                return Err(diesel::result::Error::NotFound);
            };
            claim_team(connection, team_id, membership_id, claimant).await?
        }
        None => {
            claim_organization(connection, invitation.organization_id, &offered, claimant).await?
        }
    };

    Ok(Some(Claimed {
        invitation,
        role_ids,
    }))
}

/// Hands the pre-created membership to the claimant, unless they belong already.
async fn claim_team(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    membership_id: Uuid,
    claimant: Uuid,
) -> Result<Vec<Uuid>, diesel::result::Error> {
    let already_member = TeamMembership::for_user(connection, claimant, team_id)
        .await?
        .is_some();
    if already_member {
        // The now-redundant unclaimed membership goes, and its grants with it.
        diesel::delete(
            team_memberships::table
                .filter(team_memberships::id.eq(membership_id))
                .filter(team_memberships::user_id.is_null()),
        )
        .execute(connection)
        .await?;
        return Ok(Vec::new());
    }

    diesel::update(
        team_memberships::table
            .filter(team_memberships::id.eq(membership_id))
            .filter(team_memberships::user_id.is_null()),
    )
    .set(team_memberships::user_id.eq(claimant))
    .execute(connection)
    .await?;

    grants::held_role_ids(
        connection,
        Holder::Team {
            team_id,
            membership_id,
            resource_id: None,
        },
    )
    .await
}

/// Creates the claimant's organization membership with the offered roles.
async fn claim_organization(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    offered: &[Uuid],
    claimant: Uuid,
) -> Result<Vec<Uuid>, diesel::result::Error> {
    let joined: Option<Uuid> = diesel::insert_into(organization_memberships::table)
        .values(NewOrganizationMembership {
            organization_id,
            user_id: claimant,
        })
        .on_conflict_do_nothing()
        .returning(organization_memberships::id)
        .get_result(connection)
        .await
        .optional()?;
    let Some(membership_id) = joined else {
        return Ok(Vec::new());
    };

    for role_id in offered {
        grants::grant(
            connection,
            organization_id,
            *role_id,
            Holder::Organization { membership_id },
            None,
        )
        .await?;
    }
    Ok(offered.to_vec())
}
