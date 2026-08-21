//! Tenancy endpoints: membership overview, invitations, and management.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/tenancy`). This module serves the read and invitation side:
//! `GET /memberships` lists what the caller belongs to, `GET
//! /teams/{team_id}/members` and `GET /organizations/{organization_id}/members`
//! are the two rosters, `POST /invitations` sends an invitation to a team or an
//! organization, and `POST /invitations/claim` joins the signed-in user to the
//! invitation's target. Inviting requires the admin role on the target;
//! organization admins may invite to any team in their organization.
//!
//! The management routes, which create, rename, and dissolve tenants and move
//! members between roles, live in [`super::management`] and merge in here.
//!
//! Inviting sends mail to an address the request names, so it charges the same
//! per-recipient inbox budget the auth endpoints charge; see
//! [`crate::rate_limit`].

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, Changes};
use crate::auth::CurrentUser;
use crate::auth::routes::validate_email;
use crate::billing::{Limits, PlanSet};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::{OrganizationMember, TeamMember};
use crate::http::ApiError;
use crate::mail::{Email, Mailer};
use crate::rate_limit::RateLimiter;
use crate::roles::RoleSet;
use crate::schema::{
    invitations, organization_memberships, organizations, team_memberships, teams, users,
};
use crate::tenancy::bootstrap::{DEFAULT_ROLE, holds_admin};
use crate::tenancy::invitation::{self, Invitation, InvitationTarget};
use crate::tenancy::management::lock_organization;
use crate::tenancy::model::{Organization, Team};

/// Returns the tenancy routes for an application to mount.
///
/// The role set validates requested role keys; the mailer delivers
/// invitation email; the config supplies the public base URL for links.
///
/// `plans` is the application's validated `config/billing.yml`, and it is what
/// makes the `seats` limit real: an invitation past it is refused, and a
/// membership change tells Stripe the new seat count. An application without a
/// billing file passes `None`, which is the honest reading of "no plans, no
/// limits" rather than a plan of unlimited everything invented here.
///
/// `rate_limit` is the same limiter [`crate::auth::router`] is given, so the
/// per-recipient inbox budget an invitation charges is the one a password
/// reset charges. Two limiters would be two budgets, and an inbox would take
/// both.
pub fn router(
    pool: DbPool,
    mailer: Mailer,
    roles: RoleSet,
    plans: Option<PlanSet>,
    config: &AppConfig,
    rate_limit: &RateLimiter,
) -> Router {
    Router::new()
        .route("/memberships", get(list_memberships))
        .route("/teams/{team_id}/members", get(list_team_members))
        .route(
            "/organizations/{organization_id}/members",
            get(list_organization_members),
        )
        .route("/invitations", post(create_invitation))
        .route("/invitations/claim", post(claim_invitation))
        .merge(super::management::routes())
        .with_state(TenancyState {
            pool: pool.clone(),
            mailer,
            roles: roles.clone(),
            limits: plans.map(Limits::new),
            app_url: config.app_url.clone(),
            rate_limit: rate_limit.clone(),
        })
        // CurrentUser and the guard extractors resolve their dependencies
        // from these request extensions.
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
pub(super) struct TenancyState {
    pub(super) pool: DbPool,
    pub(super) roles: RoleSet,
    mailer: Mailer,
    /// Absent for an application with no `config/billing.yml`.
    limits: Option<Limits>,
    app_url: String,
    /// The inbox budget an invitation charges, shared with the auth surface.
    rate_limit: RateLimiter,
}

impl TenancyState {
    /// Queues the seat-count update Stripe needs after a membership change.
    ///
    /// One method rather than a condition at each call site, because every
    /// place a person joins or leaves has to do this and none of them should
    /// have to know whether the application sells seats. The insert rides
    /// `connection`, so a change that rolls back queues nothing.
    ///
    /// # Errors
    /// Returns a `500` when the insert fails, which rolls the caller's
    /// transaction back: a membership change Stripe is never told about would
    /// bill the wrong number until the next one.
    pub(super) async fn queue_seat_sync(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> Result<(), ApiError> {
        let Some(limits) = self.limits.as_ref() else {
            return Ok(());
        };

        limits
            .queue_seat_sync(connection, organization_id)
            .await
            .map_err(log_internal)
    }
}

#[derive(Deserialize)]
struct CreateInvitationBody {
    email: String,
    /// Exactly one of `team_id` or `organization_id` must be set.
    team_id: Option<Uuid>,
    organization_id: Option<Uuid>,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Serialize)]
struct InvitationResponse {
    id: Uuid,
    email: String,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct InvitationBody {
    invitation: InvitationResponse,
}

#[derive(Deserialize)]
struct ClaimBody {
    token: String,
}

#[derive(Serialize)]
struct ClaimResponseBody {
    organization: Organization,
    team: Option<Team>,
}

#[derive(Serialize)]
struct MembershipTeam {
    id: Uuid,
    name: String,
    roles: Vec<String>,
}

#[derive(Serialize)]
struct MembershipOrganization {
    id: Uuid,
    name: String,
    /// Organization-level roles; empty for users who only belong to teams.
    roles: Vec<String>,
    teams: Vec<MembershipTeam>,
}

#[derive(Serialize)]
struct MembershipsBody {
    organizations: Vec<MembershipOrganization>,
}

/// Everything the signed-in user belongs to, grouped by organization.
///
/// Users can hold team memberships without an organization membership (they
/// were invited to a team only), so organizations are collected from both
/// membership tables.
async fn list_memberships(
    State(state): State<TenancyState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let team_rows: Vec<(Organization, Team, Vec<String>)> = team_memberships::table
        .inner_join(teams::table.inner_join(organizations::table))
        .filter(team_memberships::user_id.eq(user.id))
        .select((
            Organization::as_select(),
            Team::as_select(),
            team_memberships::roles,
        ))
        .order((organizations::name.asc(), teams::name.asc()))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let org_rows: Vec<(Organization, Vec<String>)> = organization_memberships::table
        .inner_join(organizations::table)
        .filter(organization_memberships::user_id.eq(user.id))
        .select((Organization::as_select(), organization_memberships::roles))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut grouped: BTreeMap<Uuid, MembershipOrganization> = BTreeMap::new();
    for (organization, roles) in org_rows {
        grouped.insert(
            organization.id,
            MembershipOrganization {
                id: organization.id,
                name: organization.name,
                roles,
                teams: Vec::new(),
            },
        );
    }
    for (organization, team, roles) in team_rows {
        let entry = grouped
            .entry(organization.id)
            .or_insert(MembershipOrganization {
                id: organization.id,
                name: organization.name,
                roles: Vec::new(),
                teams: Vec::new(),
            });
        entry.teams.push(MembershipTeam {
            id: team.id,
            name: team.name,
            roles,
        });
    }

    let mut organizations_list: Vec<MembershipOrganization> = grouped.into_values().collect();
    organizations_list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

    Ok(Json(MembershipsBody {
        organizations: organizations_list,
    }))
}

#[derive(Serialize)]
struct TeamMemberEntry {
    membership_id: Uuid,
    /// The member's email, from the account or the pending invitation.
    email: Option<String>,
    roles: Vec<String>,
    /// True for invited members who have not claimed their membership yet.
    pending: bool,
    /// The invitation to revoke, for a pending member.
    invitation_id: Option<Uuid>,
}

#[derive(Serialize)]
struct TeamMembersBody {
    members: Vec<TeamMemberEntry>,
}

/// One roster row: membership id, roles, account email, invitation, its email.
type RosterRow = (
    Uuid,
    Vec<String>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
);

/// The team's roster, visible to any member of the team.
async fn list_team_members(
    State(state): State<TenancyState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let rows: Vec<RosterRow> = team_memberships::table
        .left_join(users::table)
        .left_join(
            invitations::table
                .on(invitations::team_membership_id.eq(team_memberships::id.nullable())),
        )
        .filter(team_memberships::team_id.eq(member.team.id))
        .select((
            team_memberships::id,
            team_memberships::roles,
            users::email.nullable(),
            invitations::id.nullable(),
            invitations::email.nullable(),
        ))
        .order(team_memberships::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let members = rows
        .into_iter()
        .map(
            |(membership_id, roles, user_email, invitation_id, invited_email)| TeamMemberEntry {
                membership_id,
                pending: user_email.is_none(),
                email: user_email.or(invited_email),
                roles,
                invitation_id,
            },
        )
        .collect();

    Ok(Json(TeamMembersBody { members }))
}

#[derive(Serialize)]
struct OrganizationMemberEntry {
    /// The organization membership, absent while an invitation is unclaimed.
    ///
    /// An organization invitation creates no membership up front, unlike a
    /// team one, so a pending row has an invitation to revoke and nothing else.
    membership_id: Option<Uuid>,
    /// The member's email, from the account or the pending invitation.
    email: String,
    roles: Vec<String>,
    /// True for invited people who have not claimed their membership yet.
    pending: bool,
    /// The invitation to revoke, for a pending member.
    invitation_id: Option<Uuid>,
}

#[derive(Serialize)]
struct OrganizationMembersBody {
    members: Vec<OrganizationMemberEntry>,
}

/// The organization's roster, visible to any member of the organization.
///
/// Claimed memberships come first, then the invitations still outstanding.
/// Only organization-level invitations belong here: an invitation into one of
/// the organization's teams holds a place on that team's roster instead.
async fn list_organization_members(
    State(state): State<TenancyState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let claimed: Vec<(Uuid, Vec<String>, String)> = organization_memberships::table
        .inner_join(users::table)
        .filter(organization_memberships::organization_id.eq(member.organization.id))
        .select((
            organization_memberships::id,
            organization_memberships::roles,
            users::email,
        ))
        .order(organization_memberships::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let invited: Vec<(Uuid, String, Vec<String>)> = invitations::table
        .filter(invitations::organization_id.eq(member.organization.id))
        .filter(invitations::team_id.is_null())
        .select((invitations::id, invitations::email, invitations::roles))
        .order(invitations::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut members = Vec::with_capacity(claimed.len() + invited.len());
    for (membership_id, roles, email) in claimed {
        members.push(OrganizationMemberEntry {
            membership_id: Some(membership_id),
            email,
            roles,
            pending: false,
            invitation_id: None,
        });
    }
    for (invitation_id, email, roles) in invited {
        members.push(OrganizationMemberEntry {
            membership_id: None,
            email,
            roles,
            pending: true,
            invitation_id: Some(invitation_id),
        });
    }

    Ok(Json(OrganizationMembersBody { members }))
}

/// Invites an email address to a team or an organization.
///
/// The address goes through the same [`validate_email`] registration uses,
/// which is what keeps a line break out of the recipient of an email this
/// handler is about to send, and out of the row it stores.
///
/// This is where the plan's `seats` limit is enforced, because it is the one
/// place a person joins an organization. The check runs inside the transaction
/// that creates the invitation, after locking the organization, so two admins
/// inviting at the same instant are ordered rather than each seeing room for
/// one more and both committing.
///
/// The address is also charged the per-recipient inbox budget, because this
/// endpoint mails whoever it is pointed at. Unlike the auth endpoints, which
/// charge before they look anything up, this one charges only after the
/// inviter is shown to administer the target: charging first would let any
/// signed-in account spend a stranger's inbox budget and so keep that stranger
/// from receiving a password reset.
async fn create_invitation(
    State(state): State<TenancyState>,
    CurrentUser(inviter): CurrentUser,
    context: audit::Context,
    Json(body): Json<CreateInvitationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = validate_email(&body.email)?;
    let granted_roles = normalize_roles(&state.roles, body.roles.clone())?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (target, target_name) =
        resolve_invitation_target(&mut connection, &body, inviter.id).await?;
    state.rate_limit.check_recipient(&email)?;
    let organization_id = match target {
        InvitationTarget::Organization(organization_id)
        | InvitationTarget::Team {
            organization_id, ..
        } => organization_id,
    };

    let (invitation, token) = connection
        .transaction::<(Invitation, String), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            if let Some(limits) = state.limits.as_ref() {
                limits
                    .check_seats(transaction, organization_id, &email)
                    .await?;
            }

            let (invitation, token) =
                invitation::create(transaction, target, &email, &granted_roles, inviter.id).await?;
            state.queue_seat_sync(transaction, organization_id).await?;

            // Recorded in the team when the invitation names one, so a team
            // admin reading their own log sees who was invited into it.
            let event = audit::Event::new(audit::INVITATION_CREATED, "Invitation")
                .subject(invitation.id)
                .label(&invitation.email)
                .changes(Changes::new().field(
                    "roles",
                    Vec::<String>::new(),
                    granted_roles.clone(),
                ));
            let event = match invitation.team_id {
                Some(team_id) => event.team(team_id),
                None => event.organization(organization_id),
            };
            audit::record(transaction, &context.by(&inviter), &event).await?;

            notify_invitee(
                transaction,
                &email,
                &inviter.email,
                &target_name,
                team_id_of(target),
            )
            .await?;
            Ok((invitation, token))
        })
        .await?;

    let link = format!("{}/claim-invitation?token={token}", state.app_url);
    let mail = Email {
        to: email.clone(),
        subject: format!("You're invited to join {target_name}"),
        text_body: format!(
            "{} invited you to join {target_name}.\n\n\
             Accept within {} days: {link}\n\n\
             If you weren't expecting this, ignore this email.",
            inviter.email,
            invitation::INVITATION_TTL_DAYS,
        ),
    };
    if let Err(error) = state.mailer.send(mail).await {
        tracing::error!(
            error.message = %error,
            "failed to deliver the invitation email: {{error.message}}",
        );
    }

    let response = InvitationBody {
        invitation: InvitationResponse {
            id: invitation.id,
            email,
            expires_at: invitation.expires_at,
        },
    };
    Ok((StatusCode::CREATED, Json(response)))
}

async fn claim_invitation(
    State(state): State<TenancyState>,
    CurrentUser(claimant): CurrentUser,
    context: audit::Context,
    Json(body): Json<ClaimBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let claimed = connection
        .transaction::<_, ApiError, _>(async |transaction| {
            let claimed = invitation::claim(transaction, &body.token, claimant.id).await?;
            // A claim turns an invited seat into a held one. The count rarely
            // moves, since both spellings of the same person hold one seat, but
            // the claimant may already have been counted under another address.
            if let Some(claimed) = claimed.as_ref() {
                state
                    .queue_seat_sync(transaction, claimed.organization_id)
                    .await?;

                // Two facts, and a log that only kept one of them would answer
                // half the question: the invitation was accepted, and somebody
                // joined. The subject differs, so they are two rows.
                let joined = audit::person_label(
                    claimant.first_name.as_deref(),
                    claimant.last_name.as_deref(),
                    &claimant.email,
                );
                let context = context.by(&claimant);
                for event in [
                    audit::Event::new(audit::INVITATION_CLAIMED, "Invitation")
                        .subject(claimed.id)
                        .label(&claimed.email),
                    audit::Event::new(audit::MEMBER_ADDED, "User")
                        .subject(claimant.id)
                        .label(&joined)
                        .changes(Changes::new().field(
                            "roles",
                            Vec::<String>::new(),
                            claimed.roles.clone(),
                        )),
                ] {
                    let event = match claimed.team_id {
                        Some(team_id) => event.team(team_id),
                        None => event.organization(claimed.organization_id),
                    };
                    audit::record(transaction, &context, &event).await?;
                }

                notify_admins_of_claim(transaction, claimed, claimant.id, &claimant.email).await?;
            }
            Ok(claimed)
        })
        .await?
        .ok_or_else(|| {
            ApiError::validation("That invitation is invalid or has expired. Ask for a new one.")
        })?;

    let organization: Organization = organizations::table
        .find(claimed.organization_id)
        .select(Organization::as_select())
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    let team: Option<Team> = match claimed.team_id {
        None => None,
        Some(team_id) => teams::table
            .find(team_id)
            .select(Team::as_select())
            .first(&mut connection)
            .await
            .optional()
            .map_err(log_internal)?,
    };

    Ok(Json(ClaimResponseBody { organization, team }))
}

/// The team an invitation joins, when it joins one.
fn team_id_of(target: InvitationTarget) -> Option<Uuid> {
    match target {
        InvitationTarget::Team { team_id, .. } => Some(team_id),
        InvitationTarget::Organization(_organization_id) => None,
    }
}

/// Tells an invited address that already has an account about its invitation.
///
/// An address with no account is skipped rather than remembered: there is no
/// inbox to write to, and the email carries the invitation either way. The
/// claim token stays in that email and never reaches the notification, because
/// a token is a credential and the row would be storing it in the clear, so
/// the notice explains where to accept rather than linking to it.
async fn notify_invitee(
    connection: &mut AsyncPgConnection,
    email: &str,
    inviter_email: &str,
    target_name: &str,
    team_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let invitee: Option<Uuid> = users::table
        .filter(users::email.eq(email))
        .select(users::id)
        .first(connection)
        .await
        .optional()
        .map_err(log_internal)?;

    let Some(invitee) = invitee else {
        return Ok(());
    };

    crate::notifications::notify(
        connection,
        crate::notifications::NewNotification {
            user_id: invitee,
            team_id,
            kind: crate::notifications::INVITATION_RECEIVED,
            title: &format!("{inviter_email} invited you to join {target_name}"),
            body: Some("Accept from the link in the invitation email."),
            href: None,
        },
    )
    .await
    .map_err(log_internal)?;

    Ok(())
}

/// Tells the admins of what was joined that somebody joined it.
///
/// The claimant is left out of their own news, which matters because claiming
/// an admin invitation makes them one of the recipients this reads.
async fn notify_admins_of_claim(
    connection: &mut AsyncPgConnection,
    claimed: &invitation::Invitation,
    claimant_id: Uuid,
    claimant_email: &str,
) -> Result<(), ApiError> {
    // A team invitation is news for the team's admins, an organization one for
    // the organization's, and each names the screen its own roster is on.
    let (name, href, recipients) = if let Some(team_id) = claimed.team_id {
        let name: String = teams::table
            .find(team_id)
            .select(teams::name)
            .first(connection)
            .await
            .map_err(log_internal)?;
        let recipients = crate::notifications::team_admins(connection, team_id)
            .await
            .map_err(log_internal)?;
        (
            name,
            crate::notifications::team_settings_href(team_id),
            recipients,
        )
    } else {
        let name: String = organizations::table
            .find(claimed.organization_id)
            .select(organizations::name)
            .first(connection)
            .await
            .map_err(log_internal)?;
        let recipients =
            crate::notifications::organization_admins(connection, claimed.organization_id)
                .await
                .map_err(log_internal)?;
        (
            name,
            crate::notifications::organization_settings_href(claimed.organization_id),
            recipients,
        )
    };

    let title = format!("{claimant_email} joined {name}");
    for recipient in recipients {
        if recipient == claimant_id {
            continue;
        }
        crate::notifications::notify(
            connection,
            crate::notifications::NewNotification {
                user_id: recipient,
                team_id: claimed.team_id,
                kind: crate::notifications::INVITATION_CLAIMED,
                title: &title,
                body: None,
                href: Some(&href),
            },
        )
        .await
        .map_err(log_internal)?;
    }

    Ok(())
}

/// Resolves the invitation target, authorizes the inviter against it, and
/// returns the target's display name for the email.
async fn resolve_invitation_target(
    connection: &mut AsyncPgConnection,
    body: &CreateInvitationBody,
    inviter_id: Uuid,
) -> Result<(InvitationTarget, String), ApiError> {
    match (body.team_id, body.organization_id) {
        (Some(team_id), None) => {
            let team: Team = teams::table
                .find(team_id)
                .select(Team::as_select())
                .first(connection)
                .await
                .optional()
                .map_err(log_internal)?
                .ok_or_else(|| ApiError::validation("That team does not exist."))?;

            let team_admin = holds_admin_on_team(connection, team.id, inviter_id)
                .await
                .map_err(log_internal)?;
            let org_admin =
                holds_admin_on_organization(connection, team.organization_id, inviter_id)
                    .await
                    .map_err(log_internal)?;
            if !team_admin && !org_admin {
                return Err(not_allowed());
            }

            Ok((
                InvitationTarget::Team {
                    team_id: team.id,
                    organization_id: team.organization_id,
                },
                team.name,
            ))
        }
        (None, Some(organization_id)) => {
            let organization: Organization = organizations::table
                .find(organization_id)
                .select(Organization::as_select())
                .first(connection)
                .await
                .optional()
                .map_err(log_internal)?
                .ok_or_else(|| ApiError::validation("That organization does not exist."))?;

            let org_admin = holds_admin_on_organization(connection, organization.id, inviter_id)
                .await
                .map_err(log_internal)?;
            if !org_admin {
                return Err(not_allowed());
            }

            Ok((
                InvitationTarget::Organization(organization.id),
                organization.name,
            ))
        }
        _other => Err(ApiError::validation(
            "Set exactly one of team_id or organization_id.",
        )),
    }
}

async fn holds_admin_on_organization(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let roles: Option<Vec<String>> = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::user_id.eq(user_id))
        .select(organization_memberships::roles)
        .first(connection)
        .await
        .optional()?;

    Ok(roles.is_some_and(|held| holds_admin(&held)))
}

async fn holds_admin_on_team(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let roles: Option<Vec<String>> = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.eq(user_id))
        .select(team_memberships::roles)
        .first(connection)
        .await
        .optional()?;

    Ok(roles.is_some_and(|held| holds_admin(&held)))
}

fn not_allowed() -> ApiError {
    ApiError::forbidden("You need the admin role to invite members.")
}

/// Validates requested role keys against the application's role set.
///
/// An empty list means the baseline role, so a caller who does not care about
/// roles still lands somewhere defined rather than with none at all.
pub(super) fn normalize_roles(
    roles: &RoleSet,
    requested: Vec<String>,
) -> Result<Vec<String>, ApiError> {
    let granted = if requested.is_empty() {
        vec![DEFAULT_ROLE.to_owned()]
    } else {
        requested
    };

    for role in &granted {
        if !roles.is_defined(role) {
            return Err(ApiError::validation(format!(
                "Unknown role {role:?}. Define it in config/roles.yml first."
            )));
        }
    }

    Ok(granted)
}

pub(super) fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "tenancy request failed: {{error.message}}",
    );
    ApiError::internal()
}
