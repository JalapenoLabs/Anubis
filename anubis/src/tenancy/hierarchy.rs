//! The rules that keep managing roles from being a way to gain them.
//!
//! Holding a role-management permission lets somebody change what others may
//! do, so every change is held to four rules. Each is a pure function over a
//! resolved [`Access`], so the rules are tested without a database and every
//! endpoint asks the same question the same way.
//!
//! 1. **The hierarchy.** Nobody edits, grants, revokes, reorders, or deletes a
//!    role at or above their own rank, and nobody changes the roles of, or
//!    removes, somebody who ranks at or above them. Rank is measured where the
//!    change happens, so a grant on one team never raises anybody's standing
//!    across the organization.
//! 2. **You can only give what you hold.** Every permission a change adds to a
//!    role, or hands to a member through a role, must be one the actor holds
//!    in that place. Toggling a permission off is held to the same rule, so an
//!    edit made from a stale screen cannot quietly undo somebody else's.
//! 3. **The administrator is the top of the ladder, and exempt from it.** The
//!    administrator role is the framework's answer to "who appoints the next
//!    administrator": its holders may act on any role, the administrator role
//!    included, while nobody else may grant it, revoke it, or edit it. Every
//!    organization and team keeps at least one holder, so management can never
//!    be locked out of an organization.
//! 4. **Every reorder states the whole ladder.** A partial list is refused
//!    rather than merged, so two people reordering at once can never interleave
//!    into an order neither asked for, and no list can tie two positions.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::http::ApiError;
use crate::roles::{Permission, RoleSet, Scope};
use crate::tenancy::access::Access;
use crate::tenancy::role::Role;

/// Refuses unless `actor` may act on `role`: edit, grant, revoke, or delete it.
///
/// # Errors
/// Returns `403` for the administrator role to anybody who does not hold it,
/// and for any role at or above the actor's rank.
pub(crate) fn require_outranks_role(actor: &Access, role: &Role) -> Result<(), ApiError> {
    if actor.is_administrator() {
        return Ok(());
    }
    if role.is_administrator() {
        return Err(ApiError::forbidden(
            "Only an administrator can grant, revoke, or change the administrator role.",
        ));
    }
    if role.position >= actor.rank() {
        return Err(ApiError::forbidden(
            "You can only manage roles below your highest role.",
        ));
    }
    Ok(())
}

/// Refuses unless `actor` holds every one of `permissions` where they act.
///
/// # Errors
/// Returns `403` naming the first permission the actor does not hold.
pub(crate) fn require_holds<'a>(
    actor: &Access,
    permissions: impl IntoIterator<Item = &'a Permission>,
) -> Result<(), ApiError> {
    if actor.is_administrator() {
        return Ok(());
    }
    for permission in permissions {
        if !actor.permissions().contains(permission) {
            return Err(ApiError::forbidden(format!(
                "You can only grant permissions you hold, and you do not hold {permission}."
            )));
        }
    }
    Ok(())
}

/// Refuses unless `actor` outranks `target`, or is the same person.
///
/// Changing your own roles is held to the role rules alone: a role below your
/// rank whose permissions you hold gives you nothing you lack.
///
/// # Errors
/// Returns `403` when the target ranks at or above the actor.
pub(crate) fn require_outranks_member(
    actor: &Access,
    target: &Access,
    is_self: bool,
) -> Result<(), ApiError> {
    if is_self || actor.is_administrator() {
        return Ok(());
    }
    if target.is_administrator() || target.rank() >= actor.rank() {
        return Err(ApiError::forbidden(
            "You can only manage members who rank below you.",
        ));
    }
    Ok(())
}

/// Refuses unless `actor` may hand `role` to somebody at `tier`.
///
/// The whole question a grant or an invitation asks of one role: that it can
/// be granted there at all, that it sits below the actor, and that it carries
/// nothing the actor does not hold.
///
/// # Errors
/// Returns `400` for the default role (everybody holds it already) and for a
/// seeded role `roles.yml` does not let be granted at `tier`, and `403` from
/// [`require_outranks_role`] and [`require_holds`].
pub(crate) fn require_assignable(
    actor: &Access,
    role: &Role,
    tier: Scope,
    roles: &RoleSet,
) -> Result<(), ApiError> {
    if role.is_default() {
        return Err(ApiError::validation(
            "Everybody holds the default role already, so it is never granted.",
        ));
    }
    if !role.is_grantable_at(tier, roles) {
        return Err(ApiError::validation(format!(
            "The {} role cannot be granted at the {} level.",
            role.display_name(roles),
            tier.as_str().replace('_', "-"),
        )));
    }
    require_outranks_role(actor, role)?;
    require_holds(actor, &role.permissions(roles))
}

/// The permissions in one set and not the other, either way round.
#[must_use]
pub(crate) fn toggled<'a>(
    before: &'a BTreeSet<Permission>,
    after: &'a BTreeSet<Permission>,
) -> BTreeSet<&'a Permission> {
    before.symmetric_difference(after).collect()
}

/// Plans a reorder of every role but the two pinned ones, highest first.
///
/// `current` is every role of the organization; `requested` names every role
/// except the administrator role (pinned at the top) and the default role
/// (pinned at the bottom), each once, highest first. Answers the positions
/// that change.
///
/// # Errors
/// Returns `404` for an id that is not one of the organization's roles, `400`
/// for a list that is not exactly the movable ones, and `403` when the order
/// would move a role at or above the actor's rank.
pub(crate) fn plan_reorder(
    current: &[Role],
    requested: &[Uuid],
    actor: &Access,
) -> Result<Vec<(Uuid, i32)>, ApiError> {
    // An id that is not one of this organization's roles is answered as one
    // that does not exist, whoever's it is.
    if requested
        .iter()
        .any(|id| !current.iter().any(|role| role.id == *id))
    {
        return Err(ApiError::not_found());
    }

    let movable: Vec<&Role> = current
        .iter()
        .filter(|role| !role.is_structural())
        .collect();

    let mut seen = BTreeSet::new();
    let complete = requested.len() == movable.len()
        && requested.iter().all(|id| seen.insert(*id))
        && requested
            .iter()
            .all(|id| movable.iter().any(|role| role.id == *id));
    if !complete {
        return Err(ApiError::validation(
            "List every role except the administrator and default roles, once each, \
             highest first.",
        ));
    }

    let top = i32::try_from(movable.len()).expect("an organization has fewer than 2^31 roles");
    let mut changes = Vec::new();
    for (role_id, position) in requested.iter().zip((1..=top).rev()) {
        let Some(role) = movable.iter().find(|role| role.id == *role_id) else {
            continue;
        };
        if role.position == position {
            continue;
        }
        // A role at or above the actor stays exactly where it is, which also
        // keeps every role below the actor's rank among the positions below it.
        if !actor.is_administrator() && role.position >= actor.rank() {
            return Err(ApiError::forbidden(
                "You can only reorder roles below your highest role.",
            ));
        }
        changes.push((role.id, position));
    }

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        plan_reorder, require_holds, require_outranks_member, require_outranks_role, toggled,
    };
    use crate::http::ApiError;
    use crate::roles::{Action, MEMBERSHIP, Permission, RoleSet};
    use crate::tenancy::access::Access;
    use crate::tenancy::role::Role;

    const ROLES: &str = "
roles:
  default:
    models:
      Project: [read]
  helper:
    includes: [default]
  moderator:
    includes: [default]
    models:
      Project: [update]
      Tenancy::Membership: [update]
      Tenancy::Role: [update]
  admin:
    includes: [moderator]
";

    fn role(key: Option<&str>, position: i32, permissions: Option<&[&str]>) -> Role {
        Role {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            key: key.map(ToOwned::to_owned),
            name: key.is_none().then(|| "Custom".to_owned()),
            color: key.is_none().then(|| "#000000".to_owned()),
            position,
            permissions: permissions.map(|keys| keys.iter().map(|key| (*key).to_owned()).collect()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// An organization: default 0, helper 1, a custom role 2, moderator 3,
    /// admin 4.
    fn ladder() -> Vec<Role> {
        vec![
            role(Some("default"), 0, None),
            role(Some("helper"), 1, None),
            role(None, 2, Some(&["Project.read"])),
            role(Some("moderator"), 3, None),
            role(Some("admin"), 4, None),
        ]
    }

    fn holding(roles: &[&Role]) -> Access {
        let set = RoleSet::from_yaml(ROLES).expect("roles must parse");
        Access::from_roles(roles.iter().map(|role| (*role).clone()).collect(), &set)
    }

    fn status(result: Result<impl std::fmt::Debug, ApiError>) -> StatusCode {
        result.expect_err("must refuse").status()
    }

    #[test]
    fn nobody_acts_on_a_role_at_or_above_their_own() {
        let ladder = ladder();
        let moderator = holding(&[&ladder[0], &ladder[3]]);

        require_outranks_role(&moderator, &ladder[1]).expect("a seeded role below is theirs");
        require_outranks_role(&moderator, &ladder[2]).expect("a custom role below is theirs");
        assert_eq!(
            status(require_outranks_role(&moderator, &ladder[3])),
            StatusCode::FORBIDDEN,
            "a role at your own rank is not below you",
        );
        assert_eq!(
            status(require_outranks_role(&moderator, &ladder[4])),
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn the_administrator_is_exempt_and_nobody_else_touches_it() {
        let ladder = ladder();
        let administrator = holding(&[&ladder[0], &ladder[4]]);

        require_outranks_role(&administrator, &ladder[4])
            .expect("an administrator appoints the next one");

        // Even a reordered ladder that put a role above the administrator
        // could not make it grantable by anybody else.
        let mut high = ladder[3].clone();
        high.position = 9;
        let outranking = holding(&[&ladder[0], &high]);
        assert_eq!(
            status(require_outranks_role(&outranking, &ladder[4])),
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn only_permissions_the_actor_holds_may_be_given() {
        let ladder = ladder();
        let moderator = holding(&[&ladder[0], &ladder[3]]);

        let held = Permission::new(MEMBERSHIP, Action::Update);
        let not_held = Permission::new("Project", Action::Destroy);
        require_holds(&moderator, [&held]).expect("a held permission may be given");
        assert_eq!(
            status(require_holds(&moderator, [&held, &not_held])),
            StatusCode::FORBIDDEN,
        );

        let administrator = holding(&[&ladder[4]]);
        require_holds(&administrator, [&not_held]).expect("an administrator holds everything");
    }

    #[test]
    fn a_member_ranking_at_or_above_the_actor_is_out_of_reach() {
        let ladder = ladder();
        let moderator = holding(&[&ladder[0], &ladder[3]]);
        let peer = holding(&[&ladder[0], &ladder[3]]);
        let junior = holding(&[&ladder[0], &ladder[1]]);
        let administrator = holding(&[&ladder[0], &ladder[4]]);

        require_outranks_member(&moderator, &junior, false).expect("a junior ranks below");
        assert_eq!(
            status(require_outranks_member(&moderator, &peer, false)),
            StatusCode::FORBIDDEN,
        );
        assert_eq!(
            status(require_outranks_member(&moderator, &administrator, false)),
            StatusCode::FORBIDDEN,
        );
        require_outranks_member(&moderator, &peer, true)
            .expect("your own roles are held to the role rules alone");
        require_outranks_member(&administrator, &administrator, false)
            .expect("an administrator manages anybody");
    }

    #[test]
    fn toggling_counts_both_directions() {
        let before = [
            Permission::new("Project", Action::Read),
            Permission::new("Project", Action::Update),
        ]
        .into_iter()
        .collect();
        let after = [
            Permission::new("Project", Action::Read),
            Permission::new("Project", Action::Destroy),
        ]
        .into_iter()
        .collect();

        let changed: Vec<String> = toggled(&before, &after)
            .into_iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(changed, ["Project.update", "Project.destroy"]);
    }

    #[test]
    fn a_reorder_names_every_movable_role_exactly_once() {
        let ladder = ladder();
        let administrator = holding(&[&ladder[4]]);
        let helper = ladder[1].id;
        let custom = ladder[2].id;
        let moderator = ladder[3].id;

        for partial in [
            vec![moderator, custom],
            vec![moderator, custom, custom],
            vec![moderator, custom, helper, ladder[4].id],
        ] {
            assert_eq!(
                status(plan_reorder(&ladder, &partial, &administrator)),
                StatusCode::BAD_REQUEST,
                "{partial:?}",
            );
        }
        assert_eq!(
            status(plan_reorder(
                &ladder,
                &[moderator, custom, helper, Uuid::new_v4()],
                &administrator
            )),
            StatusCode::NOT_FOUND,
            "a role of no organization, or of another, reads as missing",
        );

        let planned = plan_reorder(&ladder, &[moderator, helper, custom], &administrator)
            .expect("a full permutation is accepted");
        assert_eq!(planned, [(helper, 2), (custom, 1)]);
    }

    #[test]
    fn a_reorder_never_moves_a_role_at_or_above_the_actor() {
        let ladder = ladder();
        let moderator_access = holding(&[&ladder[0], &ladder[3]]);
        let helper = ladder[1].id;
        let custom = ladder[2].id;
        let moderator = ladder[3].id;

        let swapped_below = plan_reorder(&ladder, &[moderator, helper, custom], &moderator_access)
            .expect("roles below the actor may swap");
        assert_eq!(swapped_below.len(), 2);

        assert_eq!(
            status(plan_reorder(
                &ladder,
                &[custom, moderator, helper],
                &moderator_access
            )),
            StatusCode::FORBIDDEN,
            "moving a role above your own is the escalation the hierarchy exists to stop",
        );
    }
}
