//! The Anubis starter application server.
//!
//! The composition root: it reads configuration, migrates the database, and
//! mounts the framework's routers alongside the application's own
//! [`account_router`]. The application itself lives in the library beside this
//! file, which is what lets the integration tests drive the real routers.

use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::{db, telemetry};
use anubis_starter::{APP_MIGRATIONS, ROLES_YML, account_router};
use axum::Router;
use axum::routing::get;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

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

    // Framework tables first: the application's reference them.
    db::run_pending_migrations(database.url())
        .await
        .expect("failed to run framework migrations");
    db::run_app_migrations(database.url(), APP_MIGRATIONS)
        .await
        .expect("failed to run application migrations");
    let pool = db::connect(database.url())
        .await
        .expect("failed to connect to the database");

    // SMTP when SMTP_URL is set, otherwise the log mailer, which prints emails
    // and their action links to the console.
    let mailer = anubis::mail::Mailer::from_config(&config).expect("invalid mail configuration");

    let app = Router::new()
        .route("/healthz", get(healthz))
        // Public profile pictures at /users/{user_id}/avatar.
        .merge(anubis::auth::avatar_router(pool.clone()))
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
        .nest("/account", account_router(&pool, &roles))
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
