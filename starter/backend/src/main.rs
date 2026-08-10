//! The Anubis starter application server.
//!
//! Serves the application's API and, in production, the built SPA assets. Today it
//! exposes a health endpoint; auth, tenancy, and the API layer arrive with milestones
//! M1 through M3.

use anubis::config::AppConfig;
use anubis::{db, telemetry};
use axum::Router;
use axum::routing::get;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() {
    let config = AppConfig::from_env().expect("invalid environment configuration");
    telemetry::init(&config).expect("failed to install the tracing subscriber");

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

    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest("/auth", anubis::auth::router(pool));

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
