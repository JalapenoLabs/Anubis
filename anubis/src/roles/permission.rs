//! Permission keys: one action on one subject, spelled `<Subject>.<action>`.
//!
//! A permission is what a runtime role carries and what a guard asks about.
//! The vocabulary is compiled, never stored as free text an organization could
//! invent: every model `roles.yml` names contributes its four actions, and the
//! framework contributes its own subjects below. An organization's role is a
//! bundle drawn from that vocabulary, and a key the vocabulary no longer holds
//! (a model removed from `roles.yml`, say) grants nothing.
//!
//! # The framework's subjects
//!
//! Four things the framework itself guards are permissions rather than fixed
//! role keys, so an organization can hand them to a role of its own:
//!
//! | Subject | Actions | Guards |
//! |---|---|---|
//! | [`MEMBERSHIP`] | `create`, `update`, `destroy` | Inviting, granting and revoking roles, removing members and revoking invitations |
//! | [`ROLE`] | `create`, `update`, `destroy` | Defining, editing, reordering, and deleting roles |
//! | [`SUBSCRIPTION`] | `update` | Spending the organization's money |
//! | [`RESOURCE_GRANT`] | `update` | Choosing which teams reach the records the organization owns |
//!
//! Their names are namespaced (`Tenancy::Membership`), which no scaffolded
//! model can be, so an application's own `Subscription` model never collides
//! with the framework's.
//!
//! Everything else the framework guards (renaming or deleting a tenant,
//! creating teams, the audit log, webhook endpoints, and platform
//! applications) stays with the administrator role. Those are deliberately
//! not delegable: a platform application's token acts as the team's
//! administrator, and a webhook endpoint receives every model's payloads, so a
//! role able to mint either could exercise permissions it does not hold.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use super::{Action, BILLING_ROLE};

/// Inviting people, granting and revoking their roles, and removing them.
pub const MEMBERSHIP: &str = "Tenancy::Membership";

/// Defining, editing, reordering, and deleting an organization's roles.
pub const ROLE: &str = "Tenancy::Role";

/// Changing what the organization pays for: checkout, the portal, reconciling.
pub const SUBSCRIPTION: &str = "Billing::Subscription";

/// Choosing which teams reach the records the organization owns directly.
///
/// Holding it is not enough on its own: sharing a kind of record also takes
/// every permission on that kind a tenant may hold, organization-wide, because
/// the team it is shared with acts on those records with its own roles and its
/// administrator's token. See `docs/tenancy.md`.
pub const RESOURCE_GRANT: &str = "Tenancy::ResourceGrant";

/// The framework's subjects and the actions each one means anything for.
///
/// `manage` on one of these expands to exactly its actions, and an action not
/// listed is refused when `roles.yml` names it, because a toggle that guards
/// nothing would mislead whoever reads the role editor.
pub(crate) const FRAMEWORK_SUBJECTS: [(&str, &[Action]); 4] = [
    (
        MEMBERSHIP,
        &[Action::Create, Action::Update, Action::Destroy],
    ),
    (ROLE, &[Action::Create, Action::Update, Action::Destroy]),
    (SUBSCRIPTION, &[Action::Update]),
    (RESOURCE_GRANT, &[Action::Update]),
];

/// Grants the framework attaches to the role keys it gives meaning.
///
/// Holding the `billing` key is what lets a member pay for the organization,
/// whether or not `roles.yml` grants it anything. Attaching it here means a
/// `roles.yml` that declares `billing` bare still pays, and no application has
/// to restate the framework's own permission.
pub(crate) const FRAMEWORK_GRANTS: [(&str, &str, &[Action]); 1] =
    [(BILLING_ROLE, SUBSCRIPTION, &[Action::Update])];

/// The actions a framework subject takes, or `None` for an application model.
pub(crate) fn framework_actions(subject: &str) -> Option<&'static [Action]> {
    FRAMEWORK_SUBJECTS
        .iter()
        .find(|(name, _actions)| *name == subject)
        .map(|(_name, actions)| *actions)
}

/// One action on one subject: the unit a role carries and a guard asks about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Permission {
    subject: String,
    action: Action,
}

impl Permission {
    /// The permission to take `action` on `subject`.
    #[must_use]
    pub fn new(subject: impl Into<String>, action: Action) -> Self {
        Self {
            subject: subject.into(),
            action,
        }
    }

    /// The model or framework subject the permission is about.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The action the permission allows.
    #[must_use]
    pub fn action(&self) -> Action {
        self.action
    }

    /// Returns `true` when this is the permission to take `action` on `subject`.
    #[must_use]
    pub fn allows(&self, action: Action, subject: &str) -> bool {
        self.action == action && self.subject == subject
    }
}

impl Display for Permission {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.subject, self.action.as_str())
    }
}

/// A string that is not a permission key's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsePermissionError {
    key: String,
}

impl Display for ParsePermissionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a permission key (expected <Subject>.<read|create|update|destroy>)",
            self.key
        )
    }
}

impl std::error::Error for ParsePermissionError {}

impl FromStr for Permission {
    type Err = ParsePermissionError;

    /// Parses `<Subject>.<action>`, splitting at the last dot.
    ///
    /// Shape only: whether the subject is in an application's vocabulary is
    /// the [`RoleSet`](super::RoleSet)'s question, not this parser's.
    fn from_str(key: &str) -> Result<Self, Self::Err> {
        let refuse = || ParsePermissionError {
            key: key.to_owned(),
        };

        let (subject, action) = key.rsplit_once('.').ok_or_else(refuse)?;
        if subject.is_empty() {
            return Err(refuse());
        }
        let action = Action::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == action)
            .ok_or_else(refuse)?;

        Ok(Self::new(subject, action))
    }
}

#[cfg(test)]
mod tests {
    use super::{Permission, SUBSCRIPTION, framework_actions};
    use crate::roles::Action;

    #[test]
    fn a_key_round_trips_through_its_spelling() {
        let permission = Permission::new("CreativeConcept", Action::Update);
        let spelled = permission.to_string();

        assert_eq!(spelled, "CreativeConcept.update");
        assert_eq!(spelled.parse::<Permission>(), Ok(permission));
    }

    #[test]
    fn a_namespaced_subject_splits_at_the_last_dot() {
        let parsed: Permission = "Billing::Subscription.update"
            .parse()
            .expect("a framework key must parse");

        assert_eq!(parsed.subject(), SUBSCRIPTION);
        assert_eq!(parsed.action(), Action::Update);
    }

    #[test]
    fn malformed_keys_are_refused() {
        for key in [
            "",
            "CreativeConcept",
            ".read",
            "CreativeConcept.manage",
            "CreativeConcept.READ",
        ] {
            assert!(key.parse::<Permission>().is_err(), "{key:?} must not parse");
        }
    }

    #[test]
    fn only_framework_subjects_have_a_fixed_action_list() {
        assert_eq!(framework_actions(SUBSCRIPTION), Some(&[Action::Update][..]));
        assert_eq!(framework_actions("Subscription"), None);
    }
}
