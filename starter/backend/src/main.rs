//! The Anubis starter application server.
//!
//! Serves the application's API and, in production, the built SPA assets. Today it
//! exposes a health endpoint; auth, tenancy, and the API layer arrive with milestones
//! M1 through M3.

use axum::Router;
use axum::routing::get;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Address the development server binds to until server config lands with M1.
const BIND_ADDRESS: &str = "127.0.0.1:3000";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = Router::new().route("/healthz", get(healthz));

    let listener = tokio::net::TcpListener::bind(BIND_ADDRESS)
        .await
        .expect("failed to bind the server address");

    tracing::info!(
        server.address = BIND_ADDRESS,
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
