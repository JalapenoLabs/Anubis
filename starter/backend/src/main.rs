//! The Anubis starter application server.
//!
//! Serves the application's API and, in production, the built SPA assets. Today it
//! exposes a health endpoint; auth, tenancy, and the API layer arrive with milestones
//! M1 through M3.

use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::{db, telemetry};
use axum::Router;
use axum::routing::get;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// The application's role definitions, embedded at compile time.
const ROLES_YML: &str = include_str!("../../config/roles.yml");

#[tokio::main]
async fn main() {
    let config = AppConfig::from_env().expect("invalid environment configuration");
    telemetry::init(&config).expect("failed to install the tracing subscriber");

    // Validated at boot so a bad roles.yml edit can never reach traffic.
    let roles = RoleSet::from_yaml(ROLES_YML).expect("config/roles.yml is invalid");
    tracing::info!(
        roles.count = roles.role_keys().count(),
        "role definitions loaded: {{roles.count}} roles",
    );

    let database = config
        .database
        .as_ref()
        .expect("DATABASE_URL is required (e.g. postgres://user:pass@localhost/app_development)");

    db::run_pending_migrations(database.url())
        .await
        .expect("failed to run database migrations");
    let pool = db::connect(database.url())
        .await
        .expect("failed to connect to the database");

    // The log mailer prints emails (and their action links) to the console.
    // Swap in a transport backend for production delivery.
    let mailer = anubis::mail::Mailer::log();

    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles.clone(), &config),
        )
        .nest(
            "/developers",
            anubis::api::management::router(pool.clone(), roles.clone()),
        )
        .nest("/api/v1", anubis::api::v1::router(pool.clone()))
        // Application routes guard with TeamMember / OrganizationMember /
        // CurrentUser through these extensions.
        .layer(anubis::guard::layer(pool, roles));

    let address = config.server.socket_addr();
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("failed to bind the server address");

    tracing::info!(
        server.address = %address,
        app.environment = %config.environment,
        framework.version = anubis::VERSION,
        "server listening on {{server.address}}",
    );

    axum::serve(listener, app)
        .await
        .expect("server terminated unexpectedly");
}

async fn healthz() -> &'static str {
    "ok"
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
}
