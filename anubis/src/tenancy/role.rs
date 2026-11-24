//! An organization's runtime roles: rows it names, colours, orders, and edits.
//!
//! A role is a bundle of permission keys drawn from the compiled vocabulary
//! (see [`crate::roles`]), with a name, a colour, and a position. Position is
//! rank: higher outranks lower, the default role is always `0`, and the
//! administrator role is always the highest. Positions stay dense, so a list
//! of an organization's roles is also the full ladder.
//!
//! # Following `roles.yml`
//!
//! A role seeded from `roles.yml` carries the key it came from, and each of its
//! name, colour, and permissions follows that key's definition while the
//! column is null. The organization's first edit to one writes the value down,
//! and from then on it is the organization's. This is what lets a model
//! scaffolded next month reach every organization's unedited roles without a
//! data migration per model, and what lets a key the application removes stop
//! granting anything the moment the file says so.
//!
//! # Seeding
//!
//! An organization is created with its two structural roles, the default role
//! and the administrator role, which is all the framework needs and all a path
//! without the application's [`RoleSet`] can know (registration, accepting a
//! platform invitation). Every other recommended role is offered the first
//! time the tenancy surface touches the organization's roles, and
//! `organizations.offered_role_keys` remembers what was offered, so a
//! recommended role an organization deleted never comes back. An offered role
//! arrives directly above the default role, the lowest rank there is, so a
//! role an application adds later never outranks anything an administrator
//! already arranged.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::roles::{
    ADMIN_ROLE, Action, DEFAULT_ROLE, MEMBERSHIP, Permission, ROLE, RoleSet, SUBSCRIPTION,
};
use crate::schema::{organizations, roles as role_rows};

/// One of an organization's roles, as stored.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = role_rows)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Role {
    /// Primary key.
    pub id: Uuid,
    /// The organization the role belongs to.
    pub organization_id: Uuid,
    /// The `roles.yml` key it was seeded from, or `None` for one the
    /// organization made.
    pub key: Option<String>,
    /// The organization's own name for it, or `None` to follow `roles.yml`.
    pub name: Option<String>,
    /// The organization's own colour for it, or `None` to follow `roles.yml`.
    pub color: Option<String>,
    /// Rank: higher outranks lower.
    pub position: i32,
    /// The organization's own permission keys, or `None` to follow `roles.yml`.
    pub permissions: Option<Vec<String>>,
    /// When the role was created.
    pub created_at: DateTime<Utc>,
    /// When the role was last updated.
    pub updated_at: DateTime<Utc>,
}

impl Role {
    /// Returns `true` for the role everybody in the organization holds.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.key.as_deref() == Some(DEFAULT_ROLE)
    }

    /// Returns `true` for the administrator role at the top.
    #[must_use]
    pub fn is_administrator(&self) -> bool {
        self.key.as_deref() == Some(ADMIN_ROLE)
    }

    /// Returns `true` for the two roles every organization keeps.
    #[must_use]
    pub fn is_structural(&self) -> bool {
        self.is_default() || self.is_administrator()
    }

    /// Returns `true` once the organization has written its own permissions.
    #[must_use]
    pub fn is_customized(&self) -> bool {
        self.permissions.is_some()
    }

    /// The permissions the role carries under `roles`.
    ///
    /// Stored keys the vocabulary no longer holds grant nothing, and a seeded
    /// role whose key left `roles.yml` grants nothing: both fail closed. The
    /// administrator role carries every permission the framework guards on top
    /// of what `roles.yml` grants its key, so no edit anywhere can leave an
    /// organization without somebody able to manage its roles.
    #[must_use]
    pub fn permissions(&self, roles: &RoleSet) -> BTreeSet<Permission> {
        let mut granted: BTreeSet<Permission> = match (&self.permissions, &self.key) {
            (Some(keys), _key) => keys
                .iter()
                .filter_map(|key| key.parse::<Permission>().ok())
                .filter(|permission| roles.is_permission(permission))
                .collect(),
            (None, Some(key)) => roles.permissions_of(key),
            (None, None) => BTreeSet::new(),
        };

        if self.is_administrator() {
            granted.extend(framework_permissions());
        }

        granted
    }

    /// The role's name: the organization's own, or what `roles.yml` calls it.
    #[must_use]
    pub fn display_name(&self, roles: &RoleSet) -> String {
        match (&self.name, &self.key) {
            (Some(name), _key) => name.clone(),
            (None, Some(key)) => roles.name_of(key),
            // A row with neither is refused by a CHECK constraint.
            (None, None) => String::new(),
        }
    }

    /// The role's colour: the organization's own, or what `roles.yml` gives it.
    #[must_use]
    pub fn display_color(&self, roles: &RoleSet) -> String {
        match (&self.color, &self.key) {
            (Some(color), _key) => color.clone(),
            (None, Some(key)) => roles.color_of(key).to_owned(),
            (None, None) => crate::roles::DEFAULT_ROLE_COLOR.to_owned(),
        }
    }

    /// The role as a screen shows it, everything resolved.
    #[must_use]
    pub fn view(&self, roles: &RoleSet) -> RoleView {
        RoleView {
            id: self.id,
            key: self.key.clone(),
            name: self.display_name(roles),
            color: self.display_color(roles),
            position: self.position,
            permissions: self
                .permissions(roles)
                .iter()
                .map(ToString::to_string)
                .collect(),
            customized: self.is_customized(),
            is_default: self.is_default(),
            is_administrator: self.is_administrator(),
        }
    }

    /// The role as a roster names it.
    #[must_use]
    pub fn summary(&self, roles: &RoleSet) -> RoleSummary {
        RoleSummary {
            id: self.id,
            name: self.display_name(roles),
            color: self.display_color(roles),
            position: self.position,
        }
    }
}

/// Every permission the framework guards, which the administrator role holds.
pub(crate) fn framework_permissions() -> impl Iterator<Item = Permission> {
    [
        (MEMBERSHIP, Action::Create),
        (MEMBERSHIP, Action::Update),
        (MEMBERSHIP, Action::Destroy),
        (ROLE, Action::Create),
        (ROLE, Action::Update),
        (ROLE, Action::Destroy),
        (SUBSCRIPTION, Action::Update),
    ]
    .into_iter()
    .map(|(subject, action)| Permission::new(subject, action))
}

/// A role with everything resolved, as the role editor reads it.
#[derive(Debug, Clone, Serialize)]
pub struct RoleView {
    /// The role's id.
    pub id: Uuid,
    /// The `roles.yml` key it was seeded from, if any.
    pub key: Option<String>,
    /// Its name.
    pub name: String,
    /// Its colour, `#rrggbb`.
    pub color: String,
    /// Its rank: higher outranks lower.
    pub position: i32,
    /// The permission keys it carries, sorted.
    pub permissions: Vec<String>,
    /// Whether the organization wrote its own permissions.
    pub customized: bool,
    /// Whether this is the role everybody holds.
    pub is_default: bool,
    /// Whether this is the administrator role.
    pub is_administrator: bool,
}

/// A role as a roster or a membership names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleSummary {
    /// The role's id.
    pub id: Uuid,
    /// Its name.
    pub name: String,
    /// Its colour, `#rrggbb`.
    pub color: String,
    /// Its rank: higher outranks lower.
    pub position: i32,
}

#[derive(Insertable)]
#[diesel(table_name = role_rows)]
pub(crate) struct NewRole<'a> {
    pub organization_id: Uuid,
    pub key: Option<&'a str>,
    pub name: Option<&'a str>,
    pub color: Option<&'a str>,
    pub position: i32,
    pub permissions: Option<&'a [String]>,
}

/// Creates an organization's default and administrator roles.
///
/// Both follow `roles.yml` from the start, which is why this needs no
/// [`RoleSet`] and can run inside registration. Returns the administrator role,
/// which the creator is granted next.
///
/// # Errors
/// Returns the underlying Diesel error when an insert fails.
pub(crate) async fn seed_structural(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Role> {
    diesel::insert_into(role_rows::table)
        .values(NewRole {
            organization_id,
            key: Some(DEFAULT_ROLE),
            name: None,
            color: None,
            position: 0,
            permissions: None,
        })
        .execute(connection)
        .await?;

    let administrator: Role = diesel::insert_into(role_rows::table)
        .values(NewRole {
            organization_id,
            key: Some(ADMIN_ROLE),
            name: None,
            color: None,
            position: 1,
            permissions: None,
        })
        .returning(Role::as_returning())
        .get_result(connection)
        .await?;

    diesel::update(organizations::table.find(organization_id))
        .set(
            organizations::offered_role_keys
                .eq(vec![ADMIN_ROLE.to_owned(), DEFAULT_ROLE.to_owned()]),
        )
        .execute(connection)
        .await?;

    Ok(administrator)
}

/// Returns `true` when the organization has been offered every recommended role.
///
/// A read without a lock, so the common case (nothing new in `roles.yml`)
/// costs one indexed lookup and never queues behind a writer.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn is_fully_offered(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    organization_id: Uuid,
) -> QueryResult<bool> {
    let offered: Vec<String> = organizations::table
        .find(organization_id)
        .select(organizations::offered_role_keys)
        .first(connection)
        .await?;

    Ok(roles
        .recommended()
        .all(|key| offered.iter().any(|held| held == key)))
}

/// Offers the recommended roles the organization has not been offered yet.
///
/// The caller holds the organization's lock, which is what keeps two first
/// requests from offering the same role twice. New roles arrive directly above
/// the default role in `roles.yml` order; everything above makes room.
///
/// # Errors
/// Returns the underlying Diesel error when a query fails.
pub(crate) async fn offer_recommended(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    organization_id: Uuid,
) -> QueryResult<()> {
    let offered: Vec<String> = organizations::table
        .find(organization_id)
        .select(organizations::offered_role_keys)
        .first(connection)
        .await?;
    let present: Vec<Option<String>> = role_rows::table
        .filter(role_rows::organization_id.eq(organization_id))
        .select(role_rows::key)
        .load(connection)
        .await?;

    let mut newly_offered = Vec::new();
    let mut arriving = Vec::new();
    for key in roles.recommended() {
        if offered.iter().any(|held| held == key) {
            continue;
        }
        newly_offered.push(key.to_owned());
        // A key with a row already (a migrated organization holds every key
        // its people held) is offered without a second row.
        if !present.iter().flatten().any(|held| held == key) {
            arriving.push(key);
        }
    }

    if newly_offered.is_empty() {
        return Ok(());
    }

    if !arriving.is_empty() {
        let room = i32::try_from(arriving.len()).expect("roles.yml declares fewer than 2^31 roles");
        // One statement, so the deferrable position constraint is checked
        // once the whole ladder has moved rather than row by row.
        diesel::update(
            role_rows::table
                .filter(role_rows::organization_id.eq(organization_id))
                .filter(role_rows::position.ge(1)),
        )
        .set(role_rows::position.eq(role_rows::position + room))
        .execute(connection)
        .await?;

        let rows: Vec<NewRole<'_>> = arriving
            .iter()
            .zip(1..)
            .map(|(key, position)| NewRole {
                organization_id,
                key: Some(key),
                name: None,
                color: None,
                position,
                permissions: None,
            })
            .collect();
        diesel::insert_into(role_rows::table)
            .values(&rows)
            .execute(connection)
            .await?;
    }

    let mut offered = offered;
    offered.extend(newly_offered);
    offered.sort();
    diesel::update(organizations::table.find(organization_id))
        .set(organizations::offered_role_keys.eq(offered))
        .execute(connection)
        .await?;

    Ok(())
}

/// The organization's roles, highest first.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(crate) async fn organization_roles(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Vec<Role>> {
    role_rows::table
        .filter(role_rows::organization_id.eq(organization_id))
        .order(role_rows::position.desc())
        .select(Role::as_select())
        .load(connection)
        .await
}

/// The organization's administrator role.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails, including when
/// the organization has no administrator role, which no path can produce.
pub(crate) async fn administrator_role(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Role> {
    role_rows::table
        .filter(role_rows::organization_id.eq(organization_id))
        .filter(role_rows::key.eq(ADMIN_ROLE))
        .select(Role::as_select())
        .first(connection)
        .await
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::Role;
    use crate::roles::{Action, MEMBERSHIP, Permission, ROLE, RoleSet};

    const ROLES: &str = "
roles:
  default:
    name: Member
    models:
      Project: [read]
  editor:
    includes: [default]
    models:
      Project: [manage]
  admin:
    includes: [editor]
";

    fn role(key: Option<&str>, permissions: Option<&[&str]>) -> Role {
        Role {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            key: key.map(ToOwned::to_owned),
            name: None,
            color: None,
            position: 1,
            permissions: permissions.map(|keys| keys.iter().map(|key| (*key).to_owned()).collect()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn an_unedited_role_follows_its_key() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");
        let editor = role(Some("editor"), None);

        assert_eq!(editor.permissions(&roles), roles.permissions_of("editor"));
        assert_eq!(editor.display_name(&roles), "Editor");
        assert!(!editor.is_customized());
    }

    #[test]
    fn an_edited_role_is_the_organizations_own() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");
        let editor = role(Some("editor"), Some(&["Project.read"]));

        assert_eq!(
            editor.permissions(&roles).into_iter().collect::<Vec<_>>(),
            [Permission::new("Project", Action::Read)],
        );
        assert!(editor.is_customized());
    }

    #[test]
    fn keys_outside_the_vocabulary_grant_nothing() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");

        let stale = role(None, Some(&["Retired.read", "not a key", "Project.update"]));
        assert_eq!(
            stale.permissions(&roles).into_iter().collect::<Vec<_>>(),
            [Permission::new("Project", Action::Update)],
            "a model removed from roles.yml grants nothing",
        );

        let orphaned = role(Some("removed"), None);
        assert!(
            orphaned.permissions(&roles).is_empty(),
            "a key that left roles.yml grants nothing",
        );
    }

    #[test]
    fn the_administrator_holds_every_framework_permission() {
        let roles = RoleSet::from_yaml(ROLES).expect("roles must parse");
        let administrator = role(Some("admin"), None);
        let held = administrator.permissions(&roles);

        assert!(held.contains(&Permission::new(ROLE, Action::Update)));
        assert!(held.contains(&Permission::new(MEMBERSHIP, Action::Destroy)));
        assert!(held.contains(&Permission::new("Project", Action::Destroy)));
        assert!(
            role(Some("editor"), None)
                .permissions(&roles)
                .iter()
                .all(|permission| permission.subject() == "Project"),
            "nobody else gets the framework's subjects for free",
        );
    }
}
