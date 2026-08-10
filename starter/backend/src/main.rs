//! The Anubis starter application server.
//!
//! Serves the application's API and, in production, the built SPA assets. Today it
//! exposes a health endpoint; auth, tenancy, and the API layer arrive with milestones
//! M1 through M3.

use anubis::config::AppConfig;
use anubis::telemetry;
use axum::Router;
use axum::routing::get;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() {
    let config = AppConfig::from_env().expect("invalid environment configuration");
    telemetry::init(&config).expect("failed to install the tracing subscriber");

    let app = Router::new().route("/healthz", get(healthz));

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
