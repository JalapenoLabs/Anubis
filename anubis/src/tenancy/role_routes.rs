//! The role editor's endpoints: an organization's roles, read and changed.
//!
//! | Route | Asks for | Effect |
//! |---|---|---|
//! | `GET /organizations/{organization_id}/roles` | member | The roles, highest first, the permission vocabulary, and the caller's standing |
//! | `GET /teams/{team_id}/roles` | team member | The same, with the caller's standing in the team |
//! | `POST /organizations/{organization_id}/roles` | `Tenancy::Role.create` | Make a role, directly above the default role |
//! | `PATCH /organizations/{organization_id}/roles/{role_id}` | `Tenancy::Role.update` | Rename, recolour, or change the permissions of a role |
//! | `DELETE /organizations/{organization_id}/roles/{role_id}` | `Tenancy::Role.destroy` | Delete a role, revoking it from everybody |
//! | `PUT /organizations/{organization_id}/role-order` | `Tenancy::Role.update` | Put the roles in a new order |
//!
//! Every change is held to [`super::hierarchy`]: nobody touches a role at or
//! above their own rank, the administrator role excepted for administrators,
//! and every permission switched on or off is one the caller holds. Each runs
//! under the organization's lock with the caller's standing read again there,
//! so two people editing one organization's roles at once are ordered, and
//! the second is judged against what the first left behind.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, patch, put};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, Changes};
use crate::guard::{OrganizationMember, TeamMember};
use crate::http::{ApiError, absent_or_null};
use crate::roles::{Action, MAX_ROLE_NAME_CHARS, Permission, ROLE, RoleSet, is_hex_color};
use crate::schema::{role_grants, roles as role_rows, team_memberships};
use crate::tenancy::access::{Access, AccessView};
use crate::tenancy::hierarchy;
use crate::tenancy::management::{lock_organization, organization_standing};
use crate::tenancy::role::{self, NewRole, Role, RoleView};
use crate::tenancy::routes::{TenancyState, log_internal, role_names};

/// Returns the role routes, merged into the tenancy router.
pub(super) fn routes() -> axum::Router<TenancyState> {
    Router::new()
        .route(
            "/organizations/{organization_id}/roles",
            get(list_roles).post(create_role),
        )
        .route(
            "/organizations/{organization_id}/roles/{role_id}",
            patch(update_role).delete(delete_role),
        )
        .route(
            "/organizations/{organization_id}/role-order",
            put(reorder_roles),
        )
        .route("/teams/{team_id}/roles", get(list_team_roles))
}

#[derive(Serialize)]
struct RolesBody {
    /// The organization's roles, highest first.
    roles: Vec<RoleView>,
    /// Every permission key a role may carry, sorted.
    permissions: Vec<String>,
    /// The caller's standing where they asked, which is what decides which
    /// roles they may grant there and, organization-wide, which roles and
    /// permissions they may change.
    viewer: AccessView,
}

#[derive(Serialize)]
struct RoleBody {
    role: RoleView,
}

#[derive(Serialize)]
struct ReorderedBody {
    /// The organization's roles in their new order, highest first.
    roles: Vec<RoleView>,
}

#[derive(Deserialize)]
struct CreateRoleBody {
    name: String,
    color: String,
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Deserialize)]
struct UpdateRoleBody {
    name: Option<String>,
    color: Option<String>,
    /// A list replaces the role's permissions; `null` returns a role seeded
    /// from `roles.yml` to following it; absent leaves them alone.
    #[expect(
        clippy::option_option,
        reason = "absent permissions and null ones are different requests"
    )]
    #[serde(default, deserialize_with = "absent_or_null")]
    permissions: Option<Option<Vec<String>>>,
}

/// What an update asks of a role's permissions.
enum PermissionsChange {
    /// Leave them as they are.
    Keep,
    /// Go back to following `roles.yml`.
    Follow,
    /// Replace them with this set.
    Replace(BTreeSet<Permission>),
}

#[derive(Deserialize)]
struct ReorderBody {
    /// Every role but the administrator and default roles, highest first.
    role_ids: Vec<Uuid>,
}

/// The organization's roles, readable by any member.
///
/// Rosters name roles, so every member needs their names and colours.
async fn list_roles(
    State(state): State<TenancyState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let roles = offered_roles(&mut connection, &state.roles, organization_id).await?;
    let viewer =
        organization_standing(&mut connection, &state.roles, &member.user, organization_id).await?;

    Ok(Json(roles_body(&state.roles, &roles, viewer.view())))
}

/// The roles of the team's organization, readable by any team member.
///
/// A team's administrators grant the organization's roles inside the team,
/// and somebody invited to the team alone holds no organization membership to
/// read them through, so the team answers too. The standing is the caller's
/// in the team, which is what decides what they may grant there.
async fn list_team_roles(
    State(state): State<TenancyState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let roles = offered_roles(&mut connection, &state.roles, member.team.organization_id).await?;

    Ok(Json(roles_body(&state.roles, &roles, member.access.view())))
}

/// The organization's roles, highest first, offering any newly recommended.
///
/// The first read after `roles.yml` recommends something new is what offers
/// it; every later read is one unlocked lookup that finds nothing to do.
async fn offered_roles(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    organization_id: Uuid,
) -> Result<Vec<Role>, ApiError> {
    if !role::is_fully_offered(connection, roles, organization_id).await? {
        connection
            .transaction::<(), ApiError, _>(async |transaction| {
                lock_organization(transaction, organization_id).await?;
                role::offer_recommended(transaction, roles, organization_id).await?;
                Ok(())
            })
            .await?;
    }

    Ok(role::organization_roles(connection, organization_id).await?)
}

fn roles_body(roles: &RoleSet, held: &[Role], viewer: AccessView) -> RolesBody {
    RolesBody {
        roles: held.iter().map(|role| role.view(roles)).collect(),
        permissions: roles.vocabulary().iter().map(ToString::to_string).collect(),
        viewer,
    }
}

/// Makes a role of the organization's own, directly above the default role.
///
/// A new role starts at the bottom of the ladder because that is the one place
/// it outranks nobody; an administrator moves it from there.
async fn create_role(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Json(body): Json<CreateRoleBody>,
) -> Result<impl IntoResponse, ApiError> {
    let name = validate_role_name(&body.name)?;
    let color = validate_color(&body.color)?;
    let permissions = parse_permissions(&state.roles, &body.permissions)?;
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let created = connection
        .transaction::<Role, ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            role::offer_recommended(transaction, &state.roles, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Create, ROLE)?;
            // The new role lands at position 1, below every role but the
            // default one, so somebody holding nothing above the default role
            // would be creating a role at their own rank.
            if !actor.is_administrator() && actor.rank() < 1 {
                return Err(ApiError::forbidden(
                    "You can only create roles below your highest role.",
                ));
            }
            hierarchy::require_holds(&actor, &permissions)?;

            diesel::update(
                role_rows::table
                    .filter(role_rows::organization_id.eq(organization_id))
                    .filter(role_rows::position.ge(1)),
            )
            .set(role_rows::position.eq(role_rows::position + 1))
            .execute(transaction)
            .await?;

            let stored: Vec<String> = permissions.iter().map(ToString::to_string).collect();
            let created: Role = diesel::insert_into(role_rows::table)
                .values(NewRole {
                    organization_id,
                    key: None,
                    name: Some(&name),
                    color: Some(&color),
                    position: 1,
                    permissions: Some(&stored),
                })
                .returning(Role::as_returning())
                .get_result(transaction)
                .await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::ROLE_CREATED, "Role")
                    .organization(organization_id)
                    .subject(created.id)
                    .label(&name)
                    .changes(
                        Changes::new()
                            .field("name", serde_json::Value::Null, name.clone())
                            .field("color", serde_json::Value::Null, color.clone())
                            .field("permissions", Vec::<String>::new(), stored),
                    ),
            )
            .await?;

            Ok(created)
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(RoleBody {
            role: created.view(&state.roles),
        }),
    ))
}

/// Renames, recolours, or changes the permissions of a role.
///
/// The administrator role's permissions are not edited, because they are
/// every permission there is; the default role's are, and every member feels
/// the change, which is why switching one on is held to what the editor holds.
async fn update_role(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, role_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateRoleBody>,
) -> Result<impl IntoResponse, ApiError> {
    let name = body.name.as_deref().map(validate_role_name).transpose()?;
    let color = body.color.as_deref().map(validate_color).transpose()?;
    let change = match &body.permissions {
        None => PermissionsChange::Keep,
        Some(None) => PermissionsChange::Follow,
        Some(Some(keys)) => PermissionsChange::Replace(parse_permissions(&state.roles, keys)?),
    };
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let updated = connection
        .transaction::<Role, ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Update, ROLE)?;

            let current = find_role(transaction, organization_id, role_id).await?;
            hierarchy::require_outranks_role(&actor, &current)?;
            require_permissions_change(&state.roles, &actor, &current, &change)?;
            let renamed = RoleChanges {
                name: name.as_deref(),
                color: color.as_deref(),
            };
            if matches!(change, PermissionsChange::Keep) && renamed.is_empty() {
                return Ok(current);
            }

            let stored: Option<Vec<String>> = match &change {
                PermissionsChange::Keep => current.permissions.clone(),
                PermissionsChange::Follow => None,
                PermissionsChange::Replace(after) => {
                    Some(after.iter().map(ToString::to_string).collect())
                }
            };
            if !matches!(change, PermissionsChange::Keep) {
                diesel::update(role_rows::table.find(current.id))
                    .set(role_rows::permissions.eq(stored))
                    .execute(transaction)
                    .await?;
            }
            if !renamed.is_empty() {
                diesel::update(role_rows::table.find(current.id))
                    .set(&renamed)
                    .execute(transaction)
                    .await?;
            }
            let updated = find_role(transaction, organization_id, current.id).await?;

            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::ROLE_UPDATED, "Role")
                    .organization(organization_id)
                    .subject(updated.id)
                    .label(&updated.display_name(&state.roles))
                    .changes(role_changes(&state.roles, &current, &updated)),
            )
            .await?;

            Ok(updated)
        })
        .await?;

    Ok(Json(RoleBody {
        role: updated.view(&state.roles),
    }))
}

/// Refuses a change to a role's permissions the actor may not make.
///
/// Every permission the change switches on or off is held to what the actor
/// holds, measured against the role as it stands under the lock, so an edit
/// made from a stale screen cannot undo somebody else's.
///
/// # Errors
/// Returns `400` for the administrator role, whose permissions are not edited,
/// and for returning a role the organization made to `roles.yml`, which it
/// never followed; `403` from [`hierarchy::require_holds`].
fn require_permissions_change(
    roles: &RoleSet,
    actor: &Access,
    current: &Role,
    change: &PermissionsChange,
) -> Result<(), ApiError> {
    let after = match change {
        PermissionsChange::Keep => return Ok(()),
        _change if current.is_administrator() => {
            return Err(ApiError::validation(
                "The administrator role holds every permission, so its permissions are not \
                 edited.",
            ));
        }
        PermissionsChange::Follow => {
            let Some(key) = current.key.as_deref() else {
                return Err(ApiError::validation(
                    "Only a role seeded from the application's recommended roles can go back \
                     to following them.",
                ));
            };
            roles.permissions_of(key)
        }
        PermissionsChange::Replace(after) => after.clone(),
    };

    let before = current.permissions(roles);
    hierarchy::require_holds(actor, hierarchy::toggled(&before, &after))
}

/// What an update moved, as the audit log records it.
fn role_changes(roles: &RoleSet, before: &Role, after: &Role) -> Changes {
    let mut recorded = Changes::new();
    if after.display_name(roles) != before.display_name(roles) {
        recorded = recorded.field(
            "name",
            before.display_name(roles),
            after.display_name(roles),
        );
    }
    if after.display_color(roles) != before.display_color(roles) {
        recorded = recorded.field(
            "color",
            before.display_color(roles),
            after.display_color(roles),
        );
    }
    let (held_before, held_after) = (before.permissions(roles), after.permissions(roles));
    if held_after != held_before {
        recorded = recorded.field("permissions", spelled(&held_before), spelled(&held_after));
    }
    recorded
}

/// Deletes a role, revoking it from everybody holding it anywhere.
///
/// Holders fall back to whatever else they hold, and to the default role
/// everybody keeps, and each is told. The roles above close the gap.
async fn delete_role(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction::<(), ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Destroy, ROLE)?;

            let doomed = find_role(transaction, organization_id, role_id).await?;
            if doomed.is_structural() {
                return Err(ApiError::validation(
                    "Every organization keeps its administrator and default roles.",
                ));
            }
            hierarchy::require_outranks_role(&actor, &doomed)?;

            let holders = holders_of(transaction, organization_id, doomed.id).await?;

            diesel::delete(role_rows::table.find(doomed.id))
                .execute(transaction)
                .await?;
            diesel::update(
                role_rows::table
                    .filter(role_rows::organization_id.eq(organization_id))
                    .filter(role_rows::position.gt(doomed.position)),
            )
            .set(role_rows::position.eq(role_rows::position - 1))
            .execute(transaction)
            .await?;

            let name = doomed.display_name(&state.roles);
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::ROLE_DESTROYED, "Role")
                    .organization(organization_id)
                    .subject(doomed.id)
                    .label(&name)
                    .changes(Changes::new().field(
                        "permissions",
                        spelled(&doomed.permissions(&state.roles)),
                        Vec::<String>::new(),
                    )),
            )
            .await?;

            let title = format!("Your roles in {} changed", member.organization.name);
            let body = format!("The {name} role was deleted.");
            let href = crate::notifications::organization_settings_href(organization_id);
            for holder in holders {
                if holder == member.user.id {
                    continue;
                }
                crate::notifications::notify(
                    transaction,
                    crate::notifications::NewNotification {
                        user_id: holder,
                        team_id: None,
                        kind: crate::notifications::MEMBERSHIP_ROLES_CHANGED,
                        title: &title,
                        body: Some(&body),
                        href: Some(&href),
                    },
                )
                .await?;
            }

            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Puts the organization's roles in the order given, highest first.
///
/// The administrator role stays at the top and the default role at the bottom;
/// the list names everything between, every one of them once. A list that
/// would move a role at or above the caller is refused whole.
async fn reorder_roles(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Json(body): Json<ReorderBody>,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let reordered = connection
        .transaction::<Vec<Role>, ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            role::offer_recommended(transaction, &state.roles, organization_id).await?;
            let actor =
                organization_standing(transaction, &state.roles, &member.user, organization_id)
                    .await?;
            actor.require(Action::Update, ROLE)?;

            let before = role::organization_roles(transaction, organization_id).await?;
            let planned = hierarchy::plan_reorder(&before, &body.role_ids, &actor)?;
            if planned.is_empty() {
                return Ok(before);
            }

            // The new positions are a permutation of the old, so the ladder
            // passes through states where two roles share a rung; checking the
            // constraint at commit is what lets it.
            diesel::sql_query("SET CONSTRAINTS roles_organization_position_unique DEFERRED")
                .execute(transaction)
                .await?;
            for (moved, position) in &planned {
                diesel::update(role_rows::table.find(*moved))
                    .set(role_rows::position.eq(*position))
                    .execute(transaction)
                    .await?;
            }

            let after = role::organization_roles(transaction, organization_id).await?;
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::ROLE_REORDERED, "Organization")
                    .organization(organization_id)
                    .subject(organization_id)
                    .label(&member.organization.name)
                    .changes(Changes::new().field(
                        "order",
                        role_names(&before, &state.roles),
                        role_names(&after, &state.roles),
                    )),
            )
            .await?;

            Ok(after)
        })
        .await?;

    Ok(Json(ReorderedBody {
        roles: reordered
            .iter()
            .map(|role| role.view(&state.roles))
            .collect(),
    }))
}

/// The name and colour an update writes; `None` leaves a column alone.
#[derive(AsChangeset)]
#[diesel(table_name = role_rows)]
struct RoleChanges<'a> {
    name: Option<&'a str>,
    color: Option<&'a str>,
}

impl RoleChanges<'_> {
    /// Returns `true` when the request named neither.
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.color.is_none()
    }
}

/// Loads one of the organization's roles, answering `404` for anything else.
async fn find_role(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    role_id: Uuid,
) -> Result<Role, ApiError> {
    role_rows::table
        .filter(role_rows::id.eq(role_id))
        .filter(role_rows::organization_id.eq(organization_id))
        .select(Role::as_select())
        .first(connection)
        .await
        .optional()
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// The accounts holding a role anywhere in the organization.
///
/// Read before the role goes, because afterwards its grants are gone too.
async fn holders_of(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    role_id: Uuid,
) -> Result<BTreeSet<Uuid>, ApiError> {
    use crate::schema::{organization_memberships, sub_tenant_memberships};

    let mut holders = BTreeSet::new();
    let through_organization: Vec<Uuid> = role_grants::table
        .inner_join(
            organization_memberships::table.on(organization_memberships::id
                .nullable()
                .eq(role_grants::organization_membership_id)),
        )
        .filter(role_grants::role_id.eq(role_id))
        .filter(role_grants::organization_id.eq(organization_id))
        .select(organization_memberships::user_id)
        .load(connection)
        .await?;
    holders.extend(through_organization);
    let through_sub_tenants: Vec<Uuid> = role_grants::table
        .inner_join(
            sub_tenant_memberships::table.on(sub_tenant_memberships::id
                .nullable()
                .eq(role_grants::sub_tenant_membership_id)),
        )
        .filter(role_grants::role_id.eq(role_id))
        .select(sub_tenant_memberships::user_id)
        .load(connection)
        .await?;
    holders.extend(through_sub_tenants);
    let through_teams: Vec<Option<Uuid>> = role_grants::table
        .inner_join(
            team_memberships::table.on(team_memberships::id
                .nullable()
                .eq(role_grants::team_membership_id)),
        )
        .filter(role_grants::role_id.eq(role_id))
        .select(team_memberships::user_id)
        .load(connection)
        .await?;
    holders.extend(through_teams.into_iter().flatten());

    Ok(holders)
}

/// Parses permission keys, refusing any outside the application's vocabulary.
fn parse_permissions(roles: &RoleSet, keys: &[String]) -> Result<BTreeSet<Permission>, ApiError> {
    keys.iter()
        .map(|key| {
            key.parse::<Permission>()
                .ok()
                .filter(|permission| roles.is_permission(permission))
                .ok_or_else(|| {
                    ApiError::validation(format!("{key:?} is not a permission a role can carry."))
                })
        })
        .collect()
}

/// Trims and bounds a role name, which rosters and notifications print.
fn validate_role_name(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_ROLE_NAME_CHARS {
        return Err(ApiError::validation(format!(
            "Give the role a name of 1 to {MAX_ROLE_NAME_CHARS} characters."
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "Give the role a name without line breaks or control characters.",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Accepts `#rrggbb` in either case, storing it in lowercase.
fn validate_color(raw: &str) -> Result<String, ApiError> {
    let lowered = raw.trim().to_ascii_lowercase();
    if is_hex_color(&lowered) {
        Ok(lowered)
    } else {
        Err(ApiError::validation(
            "Give the role a colour written as #rrggbb.",
        ))
    }
}

/// A permission set as an audit row spells it.
fn spelled(permissions: &BTreeSet<Permission>) -> Vec<String> {
    permissions.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_permissions, validate_color, validate_role_name};
    use crate::roles::RoleSet;

    #[test]
    fn colors_are_stored_lowercase_and_validated() {
        assert_eq!(validate_color(" #ABCDEF ").ok().as_deref(), Some("#abcdef"));
        validate_color("red").expect_err("a word is not a colour");
        validate_color("#abcd").expect_err("four digits are not a colour");
    }

    #[test]
    fn names_are_bounded_and_single_line() {
        assert_eq!(
            validate_role_name("  Moderator ").ok().as_deref(),
            Some("Moderator")
        );
        validate_role_name("").expect_err("a name is never empty");
        validate_role_name(&"m".repeat(51)).expect_err("a name is bounded");
        validate_role_name("Mod\nerator").expect_err("a name is one line");
    }

    #[test]
    fn only_the_vocabulary_parses() {
        let roles = RoleSet::from_yaml("roles:\n  default:\n    models:\n      Project: [read]\n")
            .expect("roles must parse");

        let parsed = parse_permissions(
            &roles,
            &[
                "Project.update".to_owned(),
                "Tenancy::Role.update".to_owned(),
            ],
        )
        .expect("known keys parse");
        assert_eq!(parsed.len(), 2);

        for unknown in ["Ghost.read", "Project.manage", "Billing::Subscription.read"] {
            assert!(
                parse_permissions(&roles, &[unknown.to_owned()]).is_err(),
                "{unknown} must be refused",
            );
        }
    }
}
