//! Tenancy endpoints: membership overview, invitations, and management.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/tenancy`). This module serves the read and invitation side:
//! `GET /memberships` lists what the caller belongs to and what they may do in
//! each place, `GET /teams/{team_id}/members` and `GET
//! /organizations/{organization_id}/members` are the two rosters, `POST
//! /invitations` sends an invitation to a team or an organization offering
//! roles by id, and `POST /invitations/claim` joins the signed-in user to the
//! invitation's target.
//!
//! The management routes, which create, rename, and dissolve tenants and grant
//! and revoke roles, live in [`super::management`], and the role editor's
//! routes in [`super::role_routes`]; both merge in here.
//!
//! Inviting sends mail to an address the request names, so it charges the same
//! per-recipient inbox budget the auth endpoints charge; see
//! [`crate::rate_limit`].

use std::collections::{BTreeMap, HashMap};

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
use crate::mail::{Email, EmailKind, Mailer};
use crate::rate_limit::RateLimiter;
use crate::roles::{Action, DEFAULT_ROLE, MEMBERSHIP, RoleSet, Scope};
use crate::schema::{
    invitations, organization_memberships, organizations, role_grants, roles as role_rows,
    team_memberships, teams, users,
};
use crate::tenancy::access::{
    Access, AccessView, resolve_organization_access, resolve_team_access,
};
use crate::tenancy::hierarchy;
use crate::tenancy::invitation::{self, Invitation, InvitationTarget};
use crate::tenancy::management::lock_organization;
use crate::tenancy::model::{Organization, Team};
use crate::tenancy::role::{Role, RoleSummary};

/// Returns the tenancy routes for an application to mount.
///
/// The role set is the compiled `roles.yml`: the permission vocabulary every
/// organization's roles draw from, and the recommended roles each is offered.
/// The mailer delivers invitation email; the config supplies the public base
/// URL for links.
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
        .merge(super::role_routes::routes())
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
    /// Roles of the target's organization to grant on claim, beyond the
    /// default role everybody holds.
    #[serde(default)]
    role_ids: Vec<Uuid>,
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
    /// What the caller may do in the team.
    #[serde(flatten)]
    access: AccessView,
}

#[derive(Serialize)]
struct MembershipOrganization {
    id: Uuid,
    name: String,
    /// Whether the caller holds an organization membership, rather than only
    /// belonging to teams inside it.
    is_member: bool,
    /// What the caller may do organization-wide; nothing for a team-only
    /// member.
    #[serde(flatten)]
    access: AccessView,
    teams: Vec<MembershipTeam>,
}

#[derive(Serialize)]
struct MembershipsBody {
    organizations: Vec<MembershipOrganization>,
}

/// A team membership the overview lists: its organization, team, and id.
type TeamRow = (Organization, Team, Uuid);

/// Everything the signed-in user belongs to, grouped by organization, with
/// what they may do in each place.
///
/// Users can hold team memberships without an organization membership (they
/// were invited to a team only), so organizations are collected from both
/// membership tables. Every grant is read in one query and every default role
/// in another, so the overview costs the same for one team or fifty.
async fn list_memberships(
    State(state): State<TenancyState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let team_rows: Vec<TeamRow> = team_memberships::table
        .inner_join(teams::table.inner_join(organizations::table))
        .filter(team_memberships::user_id.eq(user.id))
        .select((
            Organization::as_select(),
            Team::as_select(),
            team_memberships::id,
        ))
        .order((organizations::name.asc(), teams::name.asc()))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let org_rows: Vec<(Organization, Uuid)> = organization_memberships::table
        .inner_join(organizations::table)
        .filter(organization_memberships::user_id.eq(user.id))
        .select((Organization::as_select(), organization_memberships::id))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let organization_membership_ids: Vec<Uuid> = org_rows.iter().map(|row| row.1).collect();
    let team_membership_ids: Vec<Uuid> = team_rows.iter().map(|row| row.2).collect();
    let mut organization_ids: Vec<Uuid> = org_rows.iter().map(|row| row.0.id).collect();
    organization_ids.extend(team_rows.iter().map(|row| row.0.id));
    let held = HeldByMembership::load(
        &mut connection,
        &organization_membership_ids,
        &team_membership_ids,
        &organization_ids,
    )
    .await?;
    let access_of = |organization_id: Uuid, membership_id: Uuid| {
        held.access(&state.roles, organization_id, membership_id)
    };

    let mut grouped: BTreeMap<Uuid, MembershipOrganization> = BTreeMap::new();
    for (organization, membership_id) in org_rows {
        grouped.insert(
            organization.id,
            MembershipOrganization {
                id: organization.id,
                name: organization.name,
                is_member: true,
                access: access_of(organization.id, membership_id),
                teams: Vec::new(),
            },
        );
    }
    for (organization, team, membership_id) in team_rows {
        let entry = grouped
            .entry(organization.id)
            .or_insert(MembershipOrganization {
                id: organization.id,
                name: organization.name,
                is_member: false,
                access: Access::from_roles(Vec::new(), &state.roles).view(),
                teams: Vec::new(),
            });
        entry.teams.push(MembershipTeam {
            id: team.id,
            name: team.name,
            access: access_of(organization.id, membership_id),
        });
    }

    let mut organizations_list: Vec<MembershipOrganization> = grouped.into_values().collect();
    organizations_list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

    Ok(Json(MembershipsBody {
        organizations: organizations_list,
    }))
}

/// The roles a page of memberships hold, read in two queries.
struct HeldByMembership {
    /// Organization-wide and team-wide grants, keyed by the membership.
    granted: HashMap<Uuid, Vec<Role>>,
    /// Each organization's default role, keyed by organization.
    defaults: HashMap<Uuid, Role>,
}

impl HeldByMembership {
    async fn load(
        connection: &mut AsyncPgConnection,
        organization_membership_ids: &[Uuid],
        team_membership_ids: &[Uuid],
        organization_ids: &[Uuid],
    ) -> Result<Self, ApiError> {
        let rows: Vec<(Option<Uuid>, Option<Uuid>, Role)> = role_grants::table
            .inner_join(role_rows::table)
            .filter(
                role_grants::organization_membership_id
                    .eq_any(organization_membership_ids)
                    .or(role_grants::team_membership_id
                        .eq_any(team_membership_ids)
                        .and(role_grants::resource_id.is_null())),
            )
            .select((
                role_grants::organization_membership_id,
                role_grants::team_membership_id,
                Role::as_select(),
            ))
            .load(connection)
            .await
            .map_err(log_internal)?;

        let defaults = role_rows::table
            .filter(role_rows::organization_id.eq_any(organization_ids))
            .filter(role_rows::key.eq(DEFAULT_ROLE))
            .select(Role::as_select())
            .load(connection)
            .await
            .map_err(log_internal)?
            .into_iter()
            .map(|role| (role.organization_id, role))
            .collect();

        let mut granted: HashMap<Uuid, Vec<Role>> = HashMap::new();
        for (organization_membership_id, team_membership_id, role) in rows {
            if let Some(membership_id) = organization_membership_id.or(team_membership_id) {
                granted.entry(membership_id).or_default().push(role);
            }
        }
        Ok(Self { granted, defaults })
    }

    /// What one membership allows, with the organization's default role.
    fn access(&self, roles: &RoleSet, organization_id: Uuid, membership_id: Uuid) -> AccessView {
        let mut held = self
            .granted
            .get(&membership_id)
            .cloned()
            .unwrap_or_default();
        held.extend(self.defaults.get(&organization_id).cloned());
        Access::from_roles(held, roles).view()
    }
}

/// The roles one record of a team was granted to a member.
#[derive(Serialize)]
struct ResourceRoles {
    resource_id: Uuid,
    roles: Vec<RoleSummary>,
}

#[derive(Serialize)]
struct TeamMemberEntry {
    membership_id: Uuid,
    /// The member's email, from the account or the pending invitation.
    email: Option<String>,
    /// The roles granted team-wide, highest first. The default role is
    /// everybody's and is not listed.
    roles: Vec<RoleSummary>,
    /// Roles granted on single records of the team.
    resource_roles: Vec<ResourceRoles>,
    /// True for invited members who have not claimed their membership yet.
    pending: bool,
    /// The invitation to revoke, for a pending member.
    invitation_id: Option<Uuid>,
}

#[derive(Serialize)]
struct TeamMembersBody {
    members: Vec<TeamMemberEntry>,
}

/// One roster row: membership id, account email, invitation, its email.
type RosterRow = (Uuid, Option<String>, Option<Uuid>, Option<String>);

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
            users::email.nullable(),
            invitations::id.nullable(),
            invitations::email.nullable(),
        ))
        .order(team_memberships::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let granted: Vec<(Option<Uuid>, Option<Uuid>, Role)> = role_grants::table
        .inner_join(role_rows::table)
        .filter(role_grants::team_id.eq(member.team.id))
        .order(role_rows::position.desc())
        .select((
            role_grants::team_membership_id,
            role_grants::resource_id,
            Role::as_select(),
        ))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut team_wide: HashMap<Uuid, Vec<RoleSummary>> = HashMap::new();
    let mut per_record: HashMap<Uuid, BTreeMap<Uuid, Vec<RoleSummary>>> = HashMap::new();
    for (membership_id, resource_id, role) in granted {
        let Some(membership_id) = membership_id else {
            continue;
        };
        let summary = role.summary(&state.roles);
        match resource_id {
            None => team_wide.entry(membership_id).or_default().push(summary),
            Some(resource_id) => per_record
                .entry(membership_id)
                .or_default()
                .entry(resource_id)
                .or_default()
                .push(summary),
        }
    }

    let members = rows
        .into_iter()
        .map(
            |(membership_id, user_email, invitation_id, invited_email)| TeamMemberEntry {
                membership_id,
                pending: user_email.is_none(),
                email: user_email.or(invited_email),
                roles: team_wide.remove(&membership_id).unwrap_or_default(),
                resource_roles: per_record
                    .remove(&membership_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(resource_id, roles)| ResourceRoles { resource_id, roles })
                    .collect(),
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
    /// The roles held organization-wide, or offered by a pending invitation,
    /// highest first. The default role is everybody's and is not listed.
    roles: Vec<RoleSummary>,
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

    let claimed: Vec<(Uuid, String)> = organization_memberships::table
        .inner_join(users::table)
        .filter(organization_memberships::organization_id.eq(member.organization.id))
        .select((organization_memberships::id, users::email))
        .order(organization_memberships::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let invited: Vec<(Uuid, String)> = invitations::table
        .filter(invitations::organization_id.eq(member.organization.id))
        .filter(invitations::team_id.is_null())
        .select((invitations::id, invitations::email))
        .order(invitations::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let granted: Vec<(Option<Uuid>, Role)> = role_grants::table
        .inner_join(role_rows::table)
        .filter(role_grants::organization_id.eq(member.organization.id))
        .filter(role_grants::organization_membership_id.is_not_null())
        .order(role_rows::position.desc())
        .select((role_grants::organization_membership_id, Role::as_select()))
        .load(&mut connection)
        .await
        .map_err(log_internal)?;
    let mut held: HashMap<Uuid, Vec<RoleSummary>> = HashMap::new();
    for (membership_id, role) in granted {
        if let Some(membership_id) = membership_id {
            held.entry(membership_id)
                .or_default()
                .push(role.summary(&state.roles));
        }
    }

    let invitation_ids: Vec<Uuid> = invited.iter().map(|row| row.0).collect();
    let offered = offered_summaries(&mut connection, &state.roles, &invitation_ids).await?;
    let mut offered = offered;

    let mut members = Vec::with_capacity(claimed.len() + invited.len());
    for (membership_id, email) in claimed {
        members.push(OrganizationMemberEntry {
            membership_id: Some(membership_id),
            email,
            roles: held.remove(&membership_id).unwrap_or_default(),
            pending: false,
            invitation_id: None,
        });
    }
    for (invitation_id, email) in invited {
        members.push(OrganizationMemberEntry {
            membership_id: None,
            email,
            roles: offered.remove(&invitation_id).unwrap_or_default(),
            pending: true,
            invitation_id: Some(invitation_id),
        });
    }

    Ok(Json(OrganizationMembersBody { members }))
}

/// The roles each organization invitation offers, highest first.
async fn offered_summaries(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    invitation_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<RoleSummary>>, ApiError> {
    let pairs = invitation::offered_role_ids(connection, invitation_ids)
        .await
        .map_err(log_internal)?;
    let role_ids: Vec<Uuid> = pairs.iter().map(|pair| pair.1).collect();
    let by_id: HashMap<Uuid, Role> = role_rows::table
        .filter(role_rows::id.eq_any(&role_ids))
        .select(Role::as_select())
        .load(connection)
        .await
        .map_err(log_internal)?
        .into_iter()
        .map(|role| (role.id, role))
        .collect();

    let mut offered: HashMap<Uuid, Vec<Role>> = HashMap::new();
    for (invitation_id, role_id) in pairs {
        if let Some(role) = by_id.get(&role_id) {
            offered.entry(invitation_id).or_default().push(role.clone());
        }
    }
    Ok(offered
        .into_iter()
        .map(|(invitation_id, mut held)| {
            held.sort_by_key(|role| std::cmp::Reverse(role.position));
            let summaries = held.iter().map(|role| role.summary(roles)).collect();
            (invitation_id, summaries)
        })
        .collect())
}

/// What an invitation is aimed at, once it is known to exist.
enum Target {
    Team(Team),
    Organization(Organization),
}

impl Target {
    fn organization_id(&self) -> Uuid {
        match self {
            Self::Team(team) => team.organization_id,
            Self::Organization(organization) => organization.id,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Team(team) => &team.name,
            Self::Organization(organization) => &organization.name,
        }
    }

    /// The tier the offered roles are granted at.
    fn tier(&self) -> Scope {
        match self {
            Self::Team(_team) => Scope::Team,
            Self::Organization(_organization) => Scope::Organization,
        }
    }

    fn invitation_target(&self) -> InvitationTarget {
        match self {
            Self::Team(team) => InvitationTarget::Team {
                team_id: team.id,
                organization_id: team.organization_id,
            },
            Self::Organization(organization) => InvitationTarget::Organization(organization.id),
        }
    }
}

/// Invites an email address to a team or an organization, offering roles.
///
/// The inviter needs `Tenancy::Membership.create` where they invite, and
/// every role offered is held to the rules a grant is (see
/// [`super::hierarchy`]): below their rank, carrying nothing they do not hold,
/// and grantable at that tier. Into a team, the inviter's standing in the team
/// or in its organization will do, because an organization's managers have
/// always been able to staff its teams. A target the inviter cannot reach
/// answers `404`, exactly as one that does not exist.
///
/// The address goes through the same [`validate_email`] registration uses,
/// which is what keeps a line break out of the recipient of an email this
/// handler is about to send, and out of the row it stores.
///
/// This is where the plan's `seats` limit is enforced, because it is the one
/// place a person joins an organization. Every check runs inside the
/// transaction that creates the invitation, after locking the organization, so
/// two admins inviting at the same instant are ordered rather than each seeing
/// room for one more, and an inviter whose role was taken a moment ago is
/// judged by the standing they have now.
///
/// The address is charged the per-recipient inbox budget only once the inviter
/// is shown to be allowed to invite there: charging first would let any
/// signed-in account spend a stranger's inbox budget and so keep that stranger
/// from receiving a password reset.
async fn create_invitation(
    State(state): State<TenancyState>,
    CurrentUser(inviter): CurrentUser,
    context: audit::Context,
    Json(body): Json<CreateInvitationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = validate_email(&body.email)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let target = load_target(&mut connection, &body).await?;
    let organization_id = target.organization_id();

    let (invitation, token) = connection
        .transaction::<(Invitation, String), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;

            let offered = load_roles(transaction, organization_id, &body.role_ids).await?;
            let standings =
                inviter_standings(transaction, &state.roles, inviter.id, &target).await?;
            let standing = authorize_offer(&standings, &offered, target.tier(), &state.roles)?;

            // Inviting an address again replaces its pending invitation, which
            // is revoking the old offer, so it answers to the same rule.
            if let Some((replaced, pending_membership)) =
                invitation::pending_for(transaction, target.invitation_target(), &email).await?
            {
                let role_ids =
                    invitation::offered_by(transaction, replaced, pending_membership).await?;
                let replaced_offer = load_roles(transaction, organization_id, &role_ids).await?;
                hierarchy::require_outranks_offer(standing, &replaced_offer)?;
            }

            state.rate_limit.check_recipient(&email)?;
            if let Some(limits) = state.limits.as_ref() {
                limits
                    .check_seats(transaction, organization_id, &email)
                    .await?;
            }

            let role_ids: Vec<Uuid> = offered.iter().map(|role| role.id).collect();
            let (invitation, token) = invitation::create(
                transaction,
                target.invitation_target(),
                &email,
                &role_ids,
                inviter.id,
            )
            .await?;
            state.queue_seat_sync(transaction, organization_id).await?;

            // Recorded in the team when the invitation names one, so a team
            // admin reading their own log sees who was invited into it.
            let event = audit::Event::new(audit::INVITATION_CREATED, "Invitation")
                .subject(invitation.id)
                .label(&invitation.email)
                .changes(Changes::new().field(
                    "roles",
                    Vec::<String>::new(),
                    role_names(&offered, &state.roles),
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
                target.name(),
                invitation.team_id,
            )
            .await?;
            Ok((invitation, token))
        })
        .await?;

    let target_name = target.name();
    let link = format!("{}/claim-invitation?token={token}", state.app_url);
    let mail = Email::new(
        EmailKind::Invitation,
        email.clone(),
        format!("You're invited to join {target_name}"),
        format!(
            "{} invited you to join {target_name}.\n\n\
             Accept within {} days: {link}\n\n\
             If you weren't expecting this, ignore this email.",
            inviter.email,
            invitation::INVITATION_TTL_DAYS,
        ),
    )
    .with_param("link", &link)
    .with_param("inviter", &inviter.email)
    .with_param("target", target_name)
    .with_param("days", invitation::INVITATION_TTL_DAYS.to_string());
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

/// Loads the tenant an invitation names, answering `404` when there is none.
async fn load_target(
    connection: &mut AsyncPgConnection,
    body: &CreateInvitationBody,
) -> Result<Target, ApiError> {
    match (body.team_id, body.organization_id) {
        (Some(team_id), None) => teams::table
            .find(team_id)
            .select(Team::as_select())
            .first(connection)
            .await
            .optional()
            .map_err(log_internal)?
            .map(Target::Team)
            .ok_or_else(ApiError::not_found),
        (None, Some(organization_id)) => organizations::table
            .find(organization_id)
            .select(Organization::as_select())
            .first(connection)
            .await
            .optional()
            .map_err(log_internal)?
            .map(Target::Organization)
            .ok_or_else(ApiError::not_found),
        _other => Err(ApiError::validation(
            "Set exactly one of team_id or organization_id.",
        )),
    }
}

/// The roles of `organization_id` that `role_ids` names, highest first.
///
/// Answers `404` for an id that is not one of this organization's roles,
/// whether it names nothing or another organization's role, so a request
/// cannot tell the two apart.
pub(super) async fn load_roles(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    role_ids: &[Uuid],
) -> Result<Vec<Role>, ApiError> {
    let mut requested = role_ids.to_vec();
    requested.sort_unstable();
    requested.dedup();

    let found: Vec<Role> = role_rows::table
        .filter(role_rows::organization_id.eq(organization_id))
        .filter(role_rows::id.eq_any(&requested))
        .order(role_rows::position.desc())
        .select(Role::as_select())
        .load(connection)
        .await
        .map_err(log_internal)?;

    if found.len() == requested.len() {
        Ok(found)
    } else {
        Err(ApiError::not_found())
    }
}

/// Where the inviter stands that could let them invite into `target`.
///
/// Into a team: the team and its organization, whichever they reach. Into an
/// organization: the organization. Answers `404` when they reach neither.
async fn inviter_standings(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    inviter_id: Uuid,
    target: &Target,
) -> Result<Vec<Access>, ApiError> {
    let mut standings = Vec::new();
    if let Target::Team(team) = target
        && let Some((_team, _membership, access)) =
            resolve_team_access(connection, roles, inviter_id, team.id, &[])
                .await
                .map_err(log_internal)?
    {
        standings.push(access);
    }
    if let Some((_organization, _membership, access)) =
        resolve_organization_access(connection, roles, inviter_id, target.organization_id())
            .await
            .map_err(log_internal)?
    {
        standings.push(access);
    }

    if standings.is_empty() {
        return Err(ApiError::not_found());
    }
    Ok(standings)
}

/// The first standing that allows all of the offer, which then answers for
/// the rest of the invitation.
///
/// Each standing is judged whole, never mixed with another: a team's rank and
/// an organization's permissions are never combined into an authority nobody
/// holds in one place.
fn authorize_offer<'standing>(
    standings: &'standing [Access],
    offered: &[Role],
    tier: Scope,
    roles: &RoleSet,
) -> Result<&'standing Access, ApiError> {
    let mut first_refusal = None;
    for standing in standings {
        let verdict = standing.require(Action::Create, MEMBERSHIP).and_then(|()| {
            offered
                .iter()
                .try_for_each(|role| hierarchy::require_assignable(standing, role, tier, roles))
        });
        match verdict {
            Ok(()) => return Ok(standing),
            Err(refusal) => {
                first_refusal.get_or_insert(refusal);
            }
        }
    }
    Err(first_refusal.unwrap_or_else(ApiError::not_found))
}

/// The names of `held`, in the order given, for an audit row.
pub(super) fn role_names(held: &[Role], roles: &RoleSet) -> Vec<String> {
    held.iter().map(|role| role.display_name(roles)).collect()
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
                let invitation = &claimed.invitation;
                state
                    .queue_seat_sync(transaction, invitation.organization_id)
                    .await?;

                let granted =
                    load_roles(transaction, invitation.organization_id, &claimed.role_ids).await?;

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
                        .subject(invitation.id)
                        .label(&invitation.email),
                    audit::Event::new(audit::MEMBER_ADDED, "User")
                        .subject(claimant.id)
                        .label(&joined)
                        .changes(Changes::new().field(
                            "roles",
                            Vec::<String>::new(),
                            role_names(&granted, &state.roles),
                        )),
                ] {
                    let event = match invitation.team_id {
                        Some(team_id) => event.team(team_id),
                        None => event.organization(invitation.organization_id),
                    };
                    audit::record(transaction, &context, &event).await?;
                }

                notify_admins_of_claim(transaction, invitation, claimant.id, &claimant.email)
                    .await?;
            }
            Ok(claimed)
        })
        .await?
        .ok_or_else(|| {
            ApiError::validation("That invitation is invalid or has expired. Ask for a new one.")
        })?;

    let organization: Organization = organizations::table
        .find(claimed.invitation.organization_id)
        .select(Organization::as_select())
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    let team: Option<Team> = match claimed.invitation.team_id {
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

/// Tells the administrators of what was joined that somebody joined it.
///
/// The claimant is left out of their own news, which matters because claiming
/// an administrator invitation makes them one of the recipients this reads.
async fn notify_admins_of_claim(
    connection: &mut AsyncPgConnection,
    claimed: &Invitation,
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

pub(super) fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "tenancy request failed: {{error.message}}",
    );
    ApiError::internal()
}
