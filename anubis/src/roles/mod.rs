//! Role and permission definitions compiled from `config/roles.yml`.
//!
//! One YAML file defines the application's whole permission vocabulary and the
//! roles it recommends: role keys, role inheritance, per-model action grants,
//! and a display name and colour for each. [`RoleSet::from_yaml`] parses and
//! fully resolves it, rejecting unknown includes, inheritance cycles, and
//! unknown actions, so applications that embed the file with `include_str!`
//! fail at boot (and their tests fail in CI) on any bad edit.
//!
//! ```yaml
//! roles:
//!   default:
//!     name: Member
//!     models:
//!       Project: [read]
//!   editor:
//!     includes: [default]
//!     color: '#3b82f6'
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
//! # Two things the file is
//!
//! **The permission vocabulary.** Every model a tenancy role names contributes
//! its four actions, and the framework adds its own subjects (see
//! [`permission`]). An organization's roles are bundles drawn from this
//! vocabulary and nothing else, so every check stays typed in Rust and in the
//! TypeScript `anubis roles generate-ts` writes. A model only a platform role
//! names belongs to the deployment and is not in it, and of the rest an
//! organization's role carries only what some tenancy role here grants
//! ([`RoleSet::grantable`]), so the file stays the ceiling on what any tenant,
//! an administrator included, can hold.
//!
//! **The recommended roles.** An organization's roles are rows it can rename,
//! recolour, reorder, edit, and delete (see [`crate::tenancy`]). The roles
//! declared here are what each organization is offered, in the order declared,
//! least senior first. A seeded role follows its definition here until the
//! organization edits it, which is what lets a model scaffolded next month
//! reach every organization's unedited roles. Two keys are structural:
//! [`DEFAULT_ROLE`] seeds the role everybody holds and [`ADMIN_ROLE`] the
//! administrator role at the top.
//!
//! `scopes` names the tiers a role may be granted at, out of `platform`,
//! `organization`, `sub_tenant`, and `team`. Omitting it means every tenancy
//! tier, which is what a role written before either newer tier existed keeps
//! meaning. It is about where a role key may be attached, never about what it
//! grants, so it does not travel through `includes`: a role that includes
//! `billing` inherits its grants without inheriting its tier.
//!
//! `platform` is the one tier a role only reaches by naming it. The platform
//! is the deployment itself, so a role granted there operates every tenant in
//! it, and a role that never asked for that reach must not acquire it by
//! sitting in a file written before the tier existed. See [`Scope::TENANCY`].
//! Platform roles stay keys held on the account (`users.platform_roles`), and
//! [`RoleSet::can`] is how they are asked about.

pub mod permission;
mod typescript;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};

use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[doc(inline)]
pub use permission::{MEMBERSHIP, ParsePermissionError, Permission, ROLE, SUBSCRIPTION};

use permission::{FRAMEWORK_GRANTS, FRAMEWORK_SUBJECTS, framework_actions};

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

/// A tier a role may be granted at.
///
/// The tiers are the tenancy model's, outermost first, with the platform above
/// them all. Ordering the variants that way keeps the generated TypeScript and
/// every listing in the same, readable sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Held on the user, and operating the whole deployment.
    ///
    /// The platform has no roster to join through, so this is the one tier
    /// granted on the account itself rather than through a membership. See
    /// [`crate::guard::PlatformMember`].
    Platform,
    /// Granted through an organization membership.
    Organization,
    /// Granted through a sub-tenant membership.
    SubTenant,
    /// Granted through a team membership.
    Team,
}

impl Scope {
    /// Every tier that exists, outermost first.
    pub const ALL: [Self; 4] = [
        Self::Platform,
        Self::Organization,
        Self::SubTenant,
        Self::Team,
    ];

    /// The tiers inside a tenant, which is what a role naming no scopes means.
    ///
    /// The platform is deliberately absent. A role reaches it only by naming
    /// it, so a file written before the tier existed keeps its meaning and no
    /// ordinary team role silently becomes an operator's.
    pub const TENANCY: [Self; 3] = [Self::Organization, Self::SubTenant, Self::Team];

    /// The tier's YAML and TypeScript spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Organization => "organization",
            Self::SubTenant => "sub_tenant",
            Self::Team => "team",
        }
    }
}

/// Role key that operates the deployment, the platform tier's `admin`.
///
/// The tenancy tiers each have a roster an administrator is appointed through,
/// and the platform has none, so one key has to be well known: it is what
/// `ANUBIS_INITIAL_ADMIN_EMAIL` grants and what
/// [`crate::guard::PlatformMember`] is usually asked about. An application
/// that wants operators declares it in `roles.yml` with `scopes: [platform]`;
/// one that wants none leaves it out, and the platform tier stays empty.
pub const OPERATOR_ROLE: &str = "operator";

/// Role key seeding every organization's administrator role.
///
/// The administrator role sits at the top of every organization, cannot be
/// deleted, and holds every permission the framework guards on top of what
/// `roles.yml` grants this key. Its holders are exempt from the role
/// hierarchy, which is what lets an administrator appoint another. Every
/// organization and every team keeps at least one holder.
pub const ADMIN_ROLE: &str = "admin";

/// Role key seeding every organization's default role.
///
/// Everybody who reaches a tenant holds the default role there, without a
/// grant: it sits at the bottom, cannot be deleted, and is what a member with
/// no other role can do.
pub const DEFAULT_ROLE: &str = "default";

/// Role key that may spend the organization's money without administering it.
///
/// The framework grants `Billing::Subscription.update` to it, so a `roles.yml`
/// declaring `billing` with no grants of its own still lets its holders pay.
pub const BILLING_ROLE: &str = "billing";

/// The colour a role declares none of.
///
/// A neutral grey that reads on light and dark themes alike, so an
/// undecorated role never looks like it means something.
pub const DEFAULT_ROLE_COLOR: &str = "#94a3b8";

/// Longest accepted role name, in characters.
pub const MAX_ROLE_NAME_CHARS: usize = 50;

/// The `roles.yml` document, with its roles in the order they are declared.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesFile {
    #[serde(deserialize_with = "declared_in_order")]
    roles: Vec<(String, RoleDefinition)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RoleDefinition {
    includes: Vec<String>,
    models: BTreeMap<String, Vec<String>>,
    scopes: Vec<String>,
    name: Option<String>,
    color: Option<String>,
}

/// Reads the `roles` map keeping its declaration order.
///
/// The order is meaningful: it is the rank an organization's recommended roles
/// are seeded in, least senior first, so a map that sorted its keys would
/// quietly reorder every new organization.
fn declared_in_order<'de, D>(deserializer: D) -> Result<Vec<(String, RoleDefinition)>, D::Error>
where
    D: Deserializer<'de>,
{
    struct DeclaredInOrder;

    impl<'de> Visitor<'de> for DeclaredInOrder {
        type Value = Vec<(String, RoleDefinition)>;

        fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map of role keys to role definitions")
        }

        fn visit_map<Access>(self, mut map: Access) -> Result<Self::Value, Access::Error>
        where
            Access: MapAccess<'de>,
        {
            let mut roles: Vec<(String, RoleDefinition)> = Vec::new();
            while let Some((key, definition)) = map.next_entry::<String, RoleDefinition>()? {
                if roles.iter().any(|(declared, _definition)| *declared == key) {
                    return Err(Access::Error::custom(format!(
                        "role {key:?} is defined twice"
                    )));
                }
                roles.push((key, definition));
            }
            Ok(roles)
        }
    }

    deserializer.deserialize_map(DeclaredInOrder)
}

/// Per-model action grants for one resolved role.
pub type ModelGrants = BTreeMap<String, BTreeSet<Action>>;

/// The fully resolved role and permission definitions of an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSet {
    resolved: BTreeMap<String, ModelGrants>,
    scopes: BTreeMap<String, BTreeSet<Scope>>,
    names: BTreeMap<String, String>,
    colors: BTreeMap<String, String>,
    /// Role keys in declaration order, least senior first.
    order: Vec<String>,
    vocabulary: BTreeSet<Permission>,
    grantable: BTreeSet<Permission>,
}

impl RoleSet {
    /// Parses and resolves a `roles.yml` document.
    ///
    /// Resolution flattens `includes` transitively, expands `manage` into
    /// every action a subject takes, and adds the grants the framework gives
    /// the keys it gives meaning (see [`permission`]).
    ///
    /// # Errors
    /// Returns an [`Error`] on malformed YAML, a role including an undefined
    /// role, an inheritance cycle, an unknown action word, an action a
    /// framework subject does not take, an unknown scope word, a structural
    /// role narrowed with `scopes`, or a name or colour that is not one.
    pub fn from_yaml(yaml: &str) -> Result<Self, Error> {
        let file: RolesFile = serde_norway::from_str(yaml)
            .map_err(|source| Error::new(format!("roles.yml does not parse: {source}")))?;

        if file.roles.is_empty() {
            return Err(Error::new("roles.yml defines no roles".to_owned()));
        }

        let definitions: BTreeMap<&str, &RoleDefinition> = file
            .roles
            .iter()
            .map(|(key, definition)| (key.as_str(), definition))
            .collect();

        // Validate includes before resolving so error messages name the edge.
        for (key, definition) in &file.roles {
            for included in &definition.includes {
                if !definitions.contains_key(included.as_str()) {
                    return Err(Error::new(format!(
                        "role {key:?} includes undefined role {included:?}"
                    )));
                }
            }
        }

        let mut resolved = BTreeMap::new();
        let mut scopes = BTreeMap::new();
        let mut names = BTreeMap::new();
        let mut colors = BTreeMap::new();
        let mut vocabulary: BTreeSet<Permission> = FRAMEWORK_SUBJECTS
            .iter()
            .flat_map(|(subject, actions)| {
                actions
                    .iter()
                    .map(|action| Permission::new(*subject, *action))
            })
            .collect();
        let mut grantable = vocabulary.clone();

        for (key, definition) in &file.roles {
            let mut grants = ModelGrants::new();
            let mut in_progress = Vec::new();
            collect_grants(&definitions, key, &mut grants, &mut in_progress)?;

            // Attached to the key itself after its includes are resolved, so a
            // role including `billing` inherits the file's grants to it and
            // never the framework's: spending stays with the key that names it.
            for (role, subject, actions) in FRAMEWORK_GRANTS {
                if role == key {
                    grants
                        .entry(subject.to_owned())
                        .or_default()
                        .extend(actions.iter().copied());
                }
            }

            let role_scopes = collect_scopes(key, definition)?;
            // Only what a tenant's role may carry is vocabulary, and only what
            // some tenancy role grants is grantable: a model only a platform
            // role names is the deployment's, never an organization's. A model
            // declared with an empty action list still names a subject an
            // organization may want to grant.
            if role_scopes
                .iter()
                .any(|scope| Scope::TENANCY.contains(scope))
            {
                for (model, actions) in &grants {
                    vocabulary.extend(
                        vocabulary_actions(model)
                            .iter()
                            .map(|action| Permission::new(model.clone(), *action)),
                    );
                    grantable.extend(
                        actions
                            .iter()
                            .map(|action| Permission::new(model.clone(), *action)),
                    );
                }
            }

            scopes.insert(key.clone(), role_scopes);
            names.insert(key.clone(), collect_name(key, definition)?);
            colors.insert(key.clone(), collect_color(key, definition)?);
            resolved.insert(key.clone(), grants);
        }

        Ok(Self {
            resolved,
            scopes,
            names,
            colors,
            order: file
                .roles
                .into_iter()
                .map(|(key, _definition)| key)
                .collect(),
            vocabulary,
            grantable,
        })
    }

    /// Returns `true` when any of the held role keys grants the action.
    ///
    /// This asks about compiled keys, which is how the platform tier and a
    /// platform application's token are authorized. A tenant's runtime roles
    /// are asked about through [`crate::tenancy::Access`].
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

    /// The role keys an organization is offered, least senior first.
    ///
    /// Every role grantable at some tenancy tier, in the order `roles.yml`
    /// declares them. Platform-only roles are held on accounts and never
    /// become an organization's.
    pub fn recommended(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str).filter(|key| {
            Scope::TENANCY
                .into_iter()
                .any(|scope| self.is_grantable_at(key, scope))
        })
    }

    /// The resolved per-model grants of a role, if defined.
    #[must_use]
    pub fn grants(&self, role: &str) -> Option<&ModelGrants> {
        self.resolved.get(role)
    }

    /// The permissions a role key carries, empty when the key is undefined.
    ///
    /// An undefined key granting nothing is what makes a role seeded from a
    /// key the application later removed fail closed.
    #[must_use]
    pub fn permissions_of(&self, role: &str) -> BTreeSet<Permission> {
        self.resolved
            .get(role)
            .into_iter()
            .flatten()
            .flat_map(|(model, actions)| {
                actions
                    .iter()
                    .map(|action| Permission::new(model.clone(), *action))
            })
            .collect()
    }

    /// Every permission a role may carry, sorted by subject and action.
    #[must_use]
    pub fn vocabulary(&self) -> &BTreeSet<Permission> {
        &self.vocabulary
    }

    /// Every permission an organization's role may carry: what some tenancy
    /// role in the file grants, and the framework's own subjects.
    ///
    /// The ceiling even the administrator gives within. The vocabulary names
    /// every action of every tenancy model, so a role editor can lay them out;
    /// this is the part of it the application lets a tenant hold at all.
    #[must_use]
    pub fn grantable(&self) -> &BTreeSet<Permission> {
        &self.grantable
    }

    /// Returns `true` when the permission is in this application's vocabulary.
    #[must_use]
    pub fn is_permission(&self, permission: &Permission) -> bool {
        self.vocabulary.contains(permission)
    }

    /// The display name `roles.yml` gives a key, or the key made readable.
    #[must_use]
    pub fn name_of(&self, role: &str) -> String {
        self.names
            .get(role)
            .cloned()
            .unwrap_or_else(|| readable(role))
    }

    /// The colour `roles.yml` gives a key, or [`DEFAULT_ROLE_COLOR`].
    #[must_use]
    pub fn color_of(&self, role: &str) -> &str {
        self.colors
            .get(role)
            .map_or(DEFAULT_ROLE_COLOR, String::as_str)
    }

    /// The tiers a role may be granted at, if the role is defined.
    #[must_use]
    pub fn scopes(&self, role: &str) -> Option<&BTreeSet<Scope>> {
        self.scopes.get(role)
    }

    /// The role keys grantable at the platform tier, in sorted order.
    ///
    /// This is what an operator screen offers and what the initial-admin seed
    /// checks its key against, so neither has to filter the whole set itself.
    pub fn platform_role_keys(&self) -> impl Iterator<Item = &str> {
        self.role_keys()
            .filter(|role| self.is_grantable_at(role, Scope::Platform))
    }

    /// Returns `true` when the role is defined and may be granted at the tier.
    ///
    /// This is what a grant asks before attaching a seeded role, so a role the
    /// application scoped to the organization cannot be granted on a team.
    #[must_use]
    pub fn is_grantable_at(&self, role: &str, scope: Scope) -> bool {
        self.scopes
            .get(role)
            .is_some_and(|scopes| scopes.contains(&scope))
    }

    /// Renders the resolved set as the generated TypeScript module.
    #[must_use]
    pub fn to_typescript(&self) -> String {
        typescript::render(self)
    }
}

/// The actions a subject takes: a framework subject's own, or all four.
fn vocabulary_actions(subject: &str) -> &'static [Action] {
    framework_actions(subject).unwrap_or(&Action::ALL)
}

/// A key as a person would read it: `billing_admin` reads "Billing admin".
fn readable(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => spaced,
    }
}

/// Reads a role's declared scopes, defaulting to every tenancy tier.
fn collect_scopes(key: &str, definition: &RoleDefinition) -> Result<BTreeSet<Scope>, Error> {
    if definition.scopes.is_empty() {
        return Ok(Scope::TENANCY.into_iter().collect());
    }

    // The two structural roles exist in every organization and every team:
    // the default role is everybody's wherever they reach, and a team is
    // created with its creator holding the administrator role. A file that
    // narrowed either would describe tenants the framework cannot build.
    if key == DEFAULT_ROLE || key == ADMIN_ROLE {
        return Err(Error::new(format!(
            "role {key:?} is structural: every organization and team holds it, \
             so it cannot be narrowed with scopes"
        )));
    }

    definition
        .scopes
        .iter()
        .map(|word| match word.as_str() {
            "platform" => Ok(Scope::Platform),
            "organization" => Ok(Scope::Organization),
            "sub_tenant" => Ok(Scope::SubTenant),
            "team" => Ok(Scope::Team),
            unknown => Err(Error::new(format!(
                "role {key:?} names unknown scope {unknown:?} \
                 (expected platform, organization, sub_tenant, or team)"
            ))),
        })
        .collect()
}

/// Reads a role's display name, defaulting to its key made readable.
fn collect_name(key: &str, definition: &RoleDefinition) -> Result<String, Error> {
    let Some(name) = definition.name.as_deref() else {
        return Ok(readable(key));
    };

    let trimmed = name.trim();
    let valid = !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_ROLE_NAME_CHARS
        && !trimmed.chars().any(char::is_control);
    if !valid {
        return Err(Error::new(format!(
            "role {key:?} has name {name:?}; a name is 1 to {MAX_ROLE_NAME_CHARS} \
             characters without control characters"
        )));
    }

    Ok(trimmed.to_owned())
}

/// Reads a role's colour, defaulting to [`DEFAULT_ROLE_COLOR`].
fn collect_color(key: &str, definition: &RoleDefinition) -> Result<String, Error> {
    let Some(color) = definition.color.as_deref() else {
        return Ok(DEFAULT_ROLE_COLOR.to_owned());
    };

    if !is_hex_color(color) {
        return Err(Error::new(format!(
            "role {key:?} has color {color:?}; a color is #rrggbb in lowercase hex"
        )));
    }

    Ok(color.to_owned())
}

/// Returns `true` for `#rrggbb` in lowercase hex, the one spelling stored.
#[must_use]
pub fn is_hex_color(candidate: &str) -> bool {
    candidate.len() == 7
        && candidate.starts_with('#')
        && candidate[1..]
            .chars()
            .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
}

/// Depth-first grant collection with cycle detection.
fn collect_grants(
    definitions: &BTreeMap<&str, &RoleDefinition>,
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
    let definition = definitions[key];

    for included in &definition.includes {
        collect_grants(definitions, included, grants, in_progress)?;
    }

    for (model, action_words) in &definition.models {
        let takes = vocabulary_actions(model);
        let actions = grants.entry(model.clone()).or_default();
        for word in action_words {
            let named: &[Action] = match word.as_str() {
                "read" => &[Action::Read],
                "create" => &[Action::Create],
                "update" => &[Action::Update],
                "destroy" => &[Action::Destroy],
                "manage" => takes,
                unknown => {
                    return Err(Error::new(format!(
                        "role {key:?} grants unknown action {unknown:?} on model {model:?} \
                         (expected read, create, update, destroy, or manage)"
                    )));
                }
            };
            if let Some(refused) = named.iter().find(|action| !takes.contains(action)) {
                let accepted = takes
                    .iter()
                    .map(|action| action.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Error::new(format!(
                    "role {key:?} grants {:?} on {model:?}, which takes only {accepted}",
                    refused.as_str(),
                )));
            }
            actions.extend(named.iter().copied());
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
    use super::{Action, DEFAULT_ROLE_COLOR, MEMBERSHIP, Permission, RoleSet, SUBSCRIPTION, Scope};

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
    fn a_role_naming_no_scopes_may_be_granted_at_every_tenancy_tier() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        for scope in Scope::TENANCY {
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
    fn the_platform_tier_is_reached_only_by_naming_it() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        assert!(
            !set.is_grantable_at("admin", Scope::Platform),
            "a role written before the platform tier must not acquire its reach",
        );
        assert_eq!(
            set.platform_role_keys().collect::<Vec<_>>(),
            Vec::<&str>::new(),
            "a file naming no platform role leaves the tier empty",
        );

        let yaml = "
roles:
  default:
    models:
      Project: [read]
  operator:
    scopes: [platform]
    models:
      Project: [manage]
";
        let set = RoleSet::from_yaml(yaml).expect("a platform role must parse");

        assert!(set.is_grantable_at("operator", Scope::Platform));
        assert!(
            !set.is_grantable_at("operator", Scope::Team),
            "naming one tier narrows the role to it",
        );
        assert_eq!(set.platform_role_keys().collect::<Vec<_>>(), ["operator"]);
        assert_eq!(
            set.recommended().collect::<Vec<_>>(),
            ["default"],
            "a platform-only role is never an organization's",
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
  editor:
    scopes: [galaxy]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        let rendered = error.to_string();
        assert!(rendered.contains("galaxy"), "got: {rendered}");
    }

    #[test]
    fn the_structural_roles_cannot_be_narrowed() {
        for key in ["admin", "default"] {
            let yaml = format!("roles:\n  {key}:\n    scopes: [organization]\n");
            let error = RoleSet::from_yaml(&yaml).expect_err("must reject");
            let rendered = error.to_string();
            assert!(rendered.contains("structural"), "got: {rendered}");
        }
    }

    #[test]
    fn empty_and_malformed_documents_are_rejected() {
        RoleSet::from_yaml("roles: {}").expect_err("empty roles must be rejected");
        RoleSet::from_yaml("not yaml: [").expect_err("malformed yaml must be rejected");
        RoleSet::from_yaml("roles:\n  admin:\n    typo_field: 1").expect_err("unknown fields");
    }

    #[test]
    fn recommended_roles_keep_their_declared_order() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        assert_eq!(
            set.recommended().collect::<Vec<_>>(),
            ["default", "editor", "billing", "admin"],
            "declaration order is seniority, least senior first",
        );
    }

    #[test]
    fn names_and_colors_default_to_something_readable() {
        let yaml = "
roles:
  default:
    name: Member
    color: '#22c55e'
  billing_admin:
    models: {}
";
        let set = RoleSet::from_yaml(yaml).expect("must parse");

        assert_eq!(set.name_of("default"), "Member");
        assert_eq!(set.color_of("default"), "#22c55e");
        assert_eq!(set.name_of("billing_admin"), "Billing admin");
        assert_eq!(set.color_of("billing_admin"), DEFAULT_ROLE_COLOR);
        assert_eq!(
            set.name_of("ghost"),
            "Ghost",
            "an undefined key still reads"
        );
    }

    #[test]
    fn names_and_colors_are_validated() {
        for yaml in [
            "roles:\n  editor:\n    name: ''\n",
            "roles:\n  editor:\n    name: \"line\\nbreak\"\n",
            "roles:\n  editor:\n    color: red\n",
            "roles:\n  editor:\n    color: '#ABCDEF'\n",
        ] {
            RoleSet::from_yaml(yaml).expect_err(yaml);
        }
    }

    #[test]
    fn the_vocabulary_is_every_named_model_and_the_frameworks_subjects() {
        let yaml = "
roles:
  default:
    models:
      Project: [read]
      Archive: []
";
        let set = RoleSet::from_yaml(yaml).expect("must parse");

        for action in Action::ALL {
            assert!(set.is_permission(&Permission::new("Project", action)));
            assert!(
                set.is_permission(&Permission::new("Archive", action)),
                "a model named with no actions is still in the vocabulary",
            );
        }
        assert!(set.is_permission(&Permission::new(MEMBERSHIP, Action::Update)));
        assert!(set.is_permission(&Permission::new(SUBSCRIPTION, Action::Update)));
        assert!(
            !set.is_permission(&Permission::new(SUBSCRIPTION, Action::Read)),
            "a framework subject takes only the actions it guards",
        );
        assert!(!set.is_permission(&Permission::new("Ghost", Action::Read)));
    }

    #[test]
    fn framework_subjects_take_only_their_own_actions() {
        let yaml = "
roles:
  treasurer:
    models:
      Billing::Subscription: [read]
";
        let error = RoleSet::from_yaml(yaml).expect_err("must reject");
        assert!(error.to_string().contains("takes only update"), "{error}");

        let yaml = "
roles:
  moderator:
    models:
      Tenancy::Membership: [manage]
";
        let set = RoleSet::from_yaml(yaml).expect("manage expands to what the subject takes");
        let held = ["moderator"];
        assert!(set.can(&held, Action::Update, MEMBERSHIP));
        assert!(!set.can(&held, Action::Read, MEMBERSHIP));
    }

    #[test]
    fn the_billing_key_keeps_paying_for_the_organization() {
        // A roles.yml may declare `billing` with no grants of its own: the
        // framework's grant is then the whole permission.
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");

        assert!(
            set.permissions_of("billing")
                .contains(&Permission::new(SUBSCRIPTION, Action::Update)),
        );
        assert!(
            !set.permissions_of("editor")
                .contains(&Permission::new(SUBSCRIPTION, Action::Update)),
        );
    }

    #[test]
    fn spending_is_the_billing_keys_alone_and_never_inherited() {
        // The framework names the key that spends, so a role including it
        // gains the file's grants and not the framework's: `finance` reads what
        // `billing` reads, and spends nothing unless the file says so.
        let yaml = "
roles:
  billing:
    models:
      Invoice: [read]
  finance:
    includes: [billing]
    models: {}
";
        let set = RoleSet::from_yaml(yaml).expect("must parse");
        let spending = Permission::new(SUBSCRIPTION, Action::Update);

        assert!(set.permissions_of("billing").contains(&spending));
        assert!(
            set.permissions_of("finance")
                .contains(&Permission::new("Invoice", Action::Read)),
        );
        assert!(
            !set.permissions_of("finance").contains(&spending),
            "including billing does not inherit the framework's grant to it",
        );
    }

    #[test]
    fn a_tenant_holds_only_what_tenancy_roles_grant() {
        let yaml = "
roles:
  default:
    models:
      Invoice: [read]
  admin:
    includes: [default]
    models: {}
  operator:
    scopes: [platform]
    models:
      Ledger: [manage]
";
        let set = RoleSet::from_yaml(yaml).expect("must parse");

        assert!(
            set.is_permission(&Permission::new("Invoice", Action::Destroy)),
            "a tenancy model's every action is vocabulary",
        );
        assert!(
            !set.is_permission(&Permission::new("Ledger", Action::Read)),
            "a model only a platform role names is not a tenant's to carry",
        );
        assert!(
            set.grantable()
                .contains(&Permission::new("Invoice", Action::Read))
        );
        assert!(
            !set.grantable()
                .contains(&Permission::new("Invoice", Action::Destroy)),
            "no tenancy role grants it, so no organization's role may carry it",
        );
        assert!(
            set.grantable()
                .contains(&Permission::new(MEMBERSHIP, Action::Destroy)),
            "the framework's own subjects are always grantable",
        );
    }

    #[test]
    fn an_undefined_key_carries_no_permissions() {
        let set = RoleSet::from_yaml(BASELINE).expect("baseline must parse");
        assert!(set.permissions_of("removed_last_month").is_empty());
    }
}
