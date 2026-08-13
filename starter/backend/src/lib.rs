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
use utoipa::OpenApi;

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
    router = router.merge(scaffolding::merely_peripheral::router(
        pool.clone(),
        roles.clone(),
    ));
    router = router.merge(scaffolding::incidentally_linked::router(
        pool.clone(),
        roles.clone(),
    ));
    // 🐺 anubis:routes
    router
}

/// The application's own `/api/v1` routes, mounted beside the framework's.
///
/// Same models as [`account_router`], authenticated with a platform
/// application's bearer token instead of a session. One statement per model,
/// for the same reason: an inserted mount is already `rustfmt`-clean.
pub fn api_v1_router(pool: &DbPool, roles: &RoleSet) -> Router {
    let mut router = Router::new();
    router = router.merge(scaffolding::absolutely_abstract::api_router(
        pool.clone(),
        roles.clone(),
    ));
    router = router.merge(scaffolding::completely_concrete::api_router(
        pool.clone(),
        roles.clone(),
    ));
    // 🐺 anubis:api-routes
    router
}

/// The application's OpenAPI 3.1 document, and the identity of its v1 API.
///
/// The application owns the document: this derive carries the title, the
/// version, and the description a consumer reads, and the merges below add
/// the paths and schemas. The framework's half comes first (its `/team`
/// endpoint, the bearer security scheme, and the shared schemas), then one
/// line per scaffolded model. `merge` never touches `info`, so the identity
/// declared here always wins, and a version freezes with this application
/// rather than with the framework release it was generated against.
#[derive(OpenApi)]
#[openapi(info(
    title = "Anubis starter application API",
    version = "1",
    description = "The versioned public REST API. Authenticate with a \
                   platform application bearer token from the Developers \
                   section.",
))]
struct ApiDoc;

/// Builds the merged document served at `/api/v1/openapi.json` and `/docs`.
///
/// `anubis client generate-ts --from <file>` renders it as the frontend's
/// generated client, and `<binary> openapi` exports it for that.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut document = ApiDoc::openapi();
    document.merge(anubis::api::v1::openapi());
    document.merge(scaffolding::absolutely_abstract::openapi());
    document.merge(scaffolding::completely_concrete::openapi());
    // 🐺 anubis:api-docs
    document
}

#[cfg(test)]
mod tests {
    use anubis::roles::RoleSet;

    use super::{ROLES_YML, openapi};

    #[test]
    fn the_document_merges_the_framework_and_every_model() {
        let document = serde_json::to_value(openapi()).expect("the document must serialize");

        assert_eq!(
            document["info"]["title"], "Anubis starter application API",
            "the application owns the document's identity",
        );
        assert!(
            document["paths"]["/api/v1/team"].is_object(),
            "the framework's half must merge in: {document}",
        );
        for path in [
            "/api/v1/creative-concepts",
            "/api/v1/creative-concepts/{creative_concept_id}",
            "/api/v1/creative-concepts/{creative_concept_id}/tangible-things",
            "/api/v1/tangible-things/{tangible_thing_id}",
        ] {
            assert!(
                document["paths"][path].is_object(),
                "{path} must be documented: {document}",
            );
        }
        for schema in ["CreativeConceptView", "TangibleThingView", "ErrorV1"] {
            assert!(
                document["components"]["schemas"][schema].is_object(),
                "{schema} must be registered: {document}",
            );
        }
        assert!(
            document["components"]["securitySchemes"]["bearer_token"].is_object(),
            "the bearer scheme must merge in: {document}",
        );
    }

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

        for model in ["CreativeConcept", "TangibleThing", "PeripheralNotion"] {
            assert!(
                editor.contains_key(model),
                "{model} must be granted to editors in config/roles.yml",
            );
        }
    }
}
