//! Role and permission definitions compiled from `config/roles.yml`.
//!
//! One YAML file defines the application's whole permission vocabulary: role
//! keys, role inheritance, and per-model action grants. [`RoleSet::from_yaml`]
//! parses and fully resolves it, rejecting unknown includes, inheritance
//! cycles, and unknown actions, so applications that embed the file with
//! `include_str!` fail at boot (and their tests fail in CI) on any bad edit.
//!
//! The same resolved set drives both sides of the stack: the backend
//! authorizes with [`RoleSet::can`], and `anubis roles generate-ts` emits the
//! TypeScript module the SPA uses to hide controls a member cannot use.
//!
//! ```yaml
//! roles:
//!   default:
//!     models:
//!       Project: [read]
//!   editor:
//!     includes: [default]
//!     models:
//!       Project: [create, update]
//!   billing:
//!     scopes: [organization]
//!   admin:
//!     includes: [editor]
//!     models:
//!       Project: [manage]
//! ```
//!
//! `scopes` names the tenancy tiers a role may be granted at, out of
//! `organization`, `sub_tenant`, and `team`. Omitting it means every tier,
//! which is what a role written before the sub-tenant tier existed keeps
//! meaning. It is about where a role key may be attached, never about what it
//! grants, so it does not travel through `includes`: a role that includes
//! `billing` inherits its grants without inheriting its tier.

mod typescript;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};

use serde::Deserialize;

/// A concrete permission on a model.
///
/// The YAML word `manage` is shorthand that expands to every action during
/// resolution; it is not an action of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// View the model.
    Read,
    /// Create new records.
    Create,
    /// Update existing records.
    Update,
    /// Delete records.
    Destroy,
}

impl Action {
    /// Every concrete action, in canonical order.
    pub const ALL: [Self; 4] = [Self::Read, Self::Create, Self::Update, Self::Destroy];

    /// The action's YAML and TypeScript spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Update => "update",
            Self::Destroy => "destroy",
        }
    }
}

/// A tenancy tier a role may be granted at.
///
/// The tiers are the tenancy model's, in the order they nest. Ordering the
/// variants that way keeps the generated TypeScript and every listing in the
/// same, readable sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Granted through an organization membership.
    Organization,
    /// Granted through a sub-tenant membership.
    SubTenant,
    /// Granted through a team membership.
    Team,
}

impl Scope {
    /// Every tier, in the order they nest. The default for a role that names
    /// no scopes, so a file written before the tier existed keeps its meaning.
    pub const ALL: [Self; 3] = [Self::Organization, Self::SubTenant, Self::Team];

    /// The tier's YAML and TypeScript spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Organization => "organization",
            Self::SubTenant => "sub_tenant",
            Self::Team => "team",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesFile {
    roles: BTreeMap<String, RoleDefinition>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RoleDefinition {
    includes: Vec<String>,
    models: BTreeMap<String, Vec<String>>,
    scopes: Vec<String>,
}

/// Per-model action grants for one resolved role.
pub type ModelGrants = BTreeMap<String, BTreeSet<Action>>;

/// The fully resolved role and permission definitions of an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSet {
    resolved: BTreeMap<String, ModelGrants>,
    scopes: BTreeMap<String, BTreeSet<Scope>>,
}

impl RoleSet {
    /// Parses and resolves a `roles.yml` document.
    ///
    /// Resolution flattens `includes` transitively and expands `manage` into
    /// every concrete action.
    ///
    /// # Errors
    /// Returns an [`Error`] on malformed YAML, a role including an undefined
    /// role, an inheritance cycle, an unknown action word, or an unknown
    /// scope word.
    pub fn from_yaml(yaml: &str) -> Result<Self, Error> {
        let file: RolesFile = serde_norway::from_str(yaml)
            .map_err(|source| Error::new(format!("roles.yml does not parse: {source}")))?;

        if file.roles.is_empty() {
            return Err(Error::new("roles.yml defines no roles".to_owned()));
        }

        // Validate includes before resolving so error messages name the edge.
        for (key, definition) in &file.roles {
            for included in &definition.includes {
                if !file.roles.contains_key(included) {
                    return Err(Error::new(format!(
                        "role {key:?} includes undefined role {included:?}"
                    )));
                }
            }
        }

        let mut resolved = BTreeMap::new();
        let mut scopes = BTreeMap::new();
        for (key, definition) in &file.roles {
            let mut grants = ModelGrants::new();
            let mut in_progress = Vec::new();
            collect_grants(&file, key, &mut grants, &mut in_progress)?;
            resolved.insert(key.clone(), grants);
            scopes.insert(key.clone(), collect_scopes(key, definition)?);
        }

        Ok(Self { resolved, scopes })
    }

    /// Returns `true` when any of the held roles grants the action.
    pub fn can<Role: AsRef<str>>(&self, held_roles: &[Role], action: Action, model: &str) -> bool {
        held_roles.iter().any(|role| {
            self.resolved
                .get(role.as_ref())
                .and_then(|grants| grants.get(model))
                .is_some_and(|actions| actions.contains(&action))
        })
    }

    /// Returns `true` when the role key is defined.
    #[must_use]
    pub fn is_defined(&self, role: &str) -> bool {
        self.resolved.contains_key(role)
    }

    /// The defined role keys, in sorted order.
    pub fn role_keys(&self) -> impl Iterator<Item = &str> {
        self.resolved.keys().map(String::as_str)
    }

    /// The resolved per-model grants of a role, if defined.
    #[must_use]
    pub fn grants(&self, role: &str) -> Option<&ModelGrants> {
        self.resolved.get(role)
    }

    /// The tenancy tiers a role may be granted at, if the role is defined.
    #[must_use]
    pub fn scopes(&self, role: &str) -> Option<&BTreeSet<Scope>> {
        self.scopes.get(role)
    }

    /// Returns `true` when the role is defined and may be granted at the tier.
    ///
    /// This is what a roster endpoint asks before writing a role key onto a
    /// membership, so a role the application scoped to the organization cannot
    /// be attached to a team.
    #[must_use]
    pub fn is_grantable_at(&self, role: &str, scope: Scope) -> bool {
        self.scopes
            .get(role)
            .is_some_and(|scopes| scopes.contains(&scope))
    }

    /// Renders the resolved set as the generated TypeScript module.
    #[must_use]
    pub fn to_typescript(&self) -> String {
        typescript::render(&self.resolved, &self.scopes)
    }
}

/// Reads a role's declared scopes, defaulting to every tier.
fn collect_scopes(key: &str, definition: &RoleDefinition) -> Result<BTreeSet<Scope>, Error> {
    if definition.scopes.is_empty() {
        return Ok(Scope::ALL.into_iter().collect());
    }

    definition
        .scopes
        .iter()
        .map(|word| match word.as_str() {
            "organization" => Ok(Scope::Organization),
            "sub_tenant" => Ok(Scope::SubTenant),
            "team" => Ok(Scope::Team),
            unknown => Err(Error::new(format!(
                "role {key:?} names unknown scope {unknown:?} \
                 (expected organization, sub_tenant, or team)"
            ))),
        })
        .collect()
}

/// Depth-first grant collection with cycle detection.
fn collect_grants(
    file: &RolesFile,
    key: &str,
    grants: &mut ModelGrants,
    in_progress: &mut Vec<String>,
) -> Result<(), Error> {
    if in_progress.iter().any(|ancestor| ancestor == key) {
        return Err(Error::new(format!(
            "role inheritance cycle: {} -> {key}",
            in_progress.join(" -> ")
        )));
    }
    in_progress.push(key.to_owned());

    // Validated above, so the definition exists.
    let definition = &file.roles[key];

    for included in &definition.includes {
        collect_grants(file, included, grants, in_progress)?;
    }

    for (model, action_words) in &definition.models {
        let actions = grants.entry(model.clone()).or_default();
        for word in action_words {
            match word.as_str() {
                "read" => {
                    actions.insert(Action::Read);
                }
                "create" => {
                    actions.insert(Action::Create);
                }
                "update" => {
                    actions.insert(Action::Update);
                }
                "destroy" => {
                    actions.insert(Action::Destroy);
                }
                "manage" => {
                    actions.extend(Action::ALL);
                }
                unknown => {
                    return Err(Error::new(format!(
                        "role {key:?} grants unknown action {unknown:?} on model {model:?} \
                         (expected read, create, update, destroy, or manage)"
                    )));
                }
            }
        }
    }

    in_progress.pop();
    Ok(())
}

/// A roles.yml parsing or resolution failure.
#[derive(Debug)]
pub struct Error {
    message: String,
    backtrace: Backtrace,
}

impl Error {
    fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::{Action, RoleSet, Scope};

    const BASELINE: &str = "
roles:
  default:
    models:
      Project: [read]
  editor:
    includes: [default]
    models:
      Project: [create, update]
  billing:
    models: {}
  admin:
    includes: [editor, billing]
    models:
      Project: [manage]
";

    #[test]
    fn includes_flatten_transitively() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        let held = ["editor".to_owned()];
        assert!(set.can(&held, Action::Read, "Project"), "inherited read");
        assert!(set.can(&held, Action::Create, "Project"), "own create");
        assert!(!set.can(&held, Action::Destroy, "Project"), "no destroy");
    }

    #[test]
    fn manage_expands_to_every_action() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        let held = ["admin".to_owned()];
        for action in Action::ALL {
            assert!(set.can(&held, action, "Project"), "admin must {action:?}");
        }
    }

    #[test]
    fn any_held_role_may_grant() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        let held = ["billing".to_owned(), "editor".to_owned()];
        assert!(set.can(&held, Action::Update, "Project"));

        let none: [String; 0] = [];
        assert!(!set.can(&none, Action::Read, "Project"));

        let unknown = ["ghost".to_owned()];
        assert!(!set.can(&unknown, Action::Read, "Project"));
    }

    #[test]
    fn undefined_includes_are_rejected() {
        let yaml = "
roles:
  admin:
    includes: [phantom]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        let rendered = error.to_string();
        assert!(rendered.contains("phantom"), "got: {rendered}");
    }

    #[test]
    fn inheritance_cycles_are_rejected() {
        let yaml = "
roles:
  a:
    includes: [b]
  b:
    includes: [a]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        let rendered = error.to_string();
        assert!(rendered.contains("cycle"), "got: {rendered}");
    }

    #[test]
    fn unknown_actions_are_rejected() {
        let yaml = "
roles:
  admin:
    models:
      Project: [pillage]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        let rendered = error.to_string();
        assert!(rendered.contains("pillage"), "got: {rendered}");
    }

    #[test]
    fn a_role_naming_no_scopes_may_be_granted_at_every_tier() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        for scope in Scope::ALL {
            assert!(
                set.is_grantable_at("editor", scope),
                "an unscoped role must reach {scope:?}",
            );
        }
        assert!(
            !set.is_grantable_at("ghost", Scope::Team),
            "an undefined role is grantable nowhere",
        );
    }

    #[test]
    fn declared_scopes_narrow_a_role_to_those_tiers() {
        let yaml = "
roles:
  billing:
    scopes: [organization]
    models: {}
  admin:
    includes: [billing]
    models: {}
";
        let set = RoleSet::from_yaml(yaml).expect("scoped roles must parse");

        assert!(set.is_grantable_at("billing", Scope::Organization));
        assert!(!set.is_grantable_at("billing", Scope::SubTenant));
        assert!(!set.is_grantable_at("billing", Scope::Team));
        assert!(
            set.is_grantable_at("admin", Scope::Team),
            "scopes describe where a key attaches, so they do not travel through includes",
        );
    }

    #[test]
    fn unknown_scopes_are_rejected() {
        let yaml = "
roles:
  admin:
    scopes: [galaxy]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        let rendered = error.to_string();
        assert!(rendered.contains("galaxy"), "got: {rendered}");
    }

    #[test]
    fn empty_and_malformed_documents_are_rejected() {
        RoleSet::from_yaml("roles: {}").expect_err("empty roles must be rejected");
        RoleSet::from_yaml("not yaml: [").expect_err("malformed yaml must be rejected");
        RoleSet::from_yaml("roles:\n  admin:\n    typo_field: 1").expect_err("unknown fields");
    }
}
