//! The application: its domain models, routes, migrations, and permissions.
//!
//! `main.rs` is the composition root that boots a server around this library.
//! Keeping the application itself in a library is what lets `tests/` drive the
//! real routers, so the living templates in [`scaffolding`] stay honest.

pub mod scaffolding;
mod schema;
// 🐺 anubis:modules

use anubis::db::DbPool;
use anubis::roles::RoleSet;
use axum::Router;
use diesel_migrations::{EmbeddedMigrations, embed_migrations};

/// The application's role definitions, embedded at compile time.
pub const ROLES_YML: &str = include_str!("../../config/roles.yml");

/// The application's own migrations, compiled into the binary.
///
/// Apply them with `anubis::db::run_app_migrations` after the framework's:
/// application tables reference `teams`, and the shared `set_updated_at()`
/// trigger function must already exist.
pub const APP_MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

/// The application's own routes, mounted at `/account`.
///
/// One statement per model, so `anubis scaffold model` can insert a new mount
/// above the anchor and have the result already be `rustfmt`-clean. Keep the
/// anchor where it is.
pub fn account_router(pool: &DbPool, roles: &RoleSet) -> Router {
    let mut router = Router::new();
    router = router.merge(scaffolding::absolutely_abstract::router(
        pool.clone(),
        roles.clone(),
    ));
    router = router.merge(scaffolding::completely_concrete::router(
        pool.clone(),
        roles.clone(),
    ));
    // 🐺 anubis:routes
    router
}

#[cfg(test)]
mod tests {
    use anubis::roles::RoleSet;

    use super::ROLES_YML;

    #[test]
    fn the_embedded_roles_file_is_valid() {
        let set = RoleSet::from_yaml(ROLES_YML).expect("config/roles.yml must be valid");
        for role in ["default", "editor", "billing", "admin"] {
            assert!(set.is_defined(role), "baseline role {role:?} must exist");
        }
    }

    #[test]
    fn every_scaffolded_model_is_granted_permissions() {
        let set = RoleSet::from_yaml(ROLES_YML).expect("config/roles.yml must be valid");
        let editor = set.grants("editor").expect("editor must be defined");

        for model in ["CreativeConcept", "TangibleThing"] {
            assert!(
                editor.contains_key(model),
                "{model} must be granted to editors in config/roles.yml",
            );
        }
    }
}
