//! The Anubis starter application server.
//!
//! The composition root: it reads configuration, migrates the database, and
//! mounts the framework's routers alongside the application's own
//! [`account_router`], with the built frontend behind them all. The
//! application itself lives in the library beside this file, which is what
//! lets the integration tests drive the real routers.
//!
//! Everything a production server needs around that router, the probes,
//! request ids, request logging, timeouts, security headers, and a graceful
//! shutdown, comes from the one call to [`anubis::server::serve`]. See
//! `docs/server.md`.

use anubis::billing::PlanSet;
use anubis::config::AppConfig;
use anubis::roles::RoleSet;
use anubis::{db, server, telemetry};
use anubis_starter::{
    APP_MIGRATIONS, BILLING_YML, ROLES_YML, account_router, api_v1_router, openapi, register_jobs,
    webhooks_router,
};
use axum::Router;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// The one argument the binary answers instead of serving.
///
/// The merged OpenAPI document belongs to the application, so the application
/// is what exports it: `anubis client generate-ts --from <file>` renders that
/// export as the frontend's generated client, and CI fails on drift. Answering
/// it here, before any configuration is read, keeps the export runnable
/// without a database.
const EXPORT_OPENAPI: &str = "openapi";

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some(EXPORT_OPENAPI) {
        let document =
            serde_json::to_string_pretty(&openapi()).expect("the OpenAPI document must serialize");
        println!("{document}");
        return;
    }

    let config = AppConfig::from_env().expect("invalid environment configuration");
    telemetry::init(&config).expect("failed to install the tracing subscriber");

    // Validated at boot so a bad roles.yml edit can never reach traffic.
    let roles = RoleSet::from_yaml(ROLES_YML).expect("config/roles.yml is invalid");
    tracing::info!(
        roles.count = roles.role_keys().count(),
        "role definitions loaded: {{roles.count}} roles",
    );

    // Same discipline as roles: a bad billing.yml edit stops the boot rather
    // than a customer's checkout.
    let plans = PlanSet::from_yaml(BILLING_YML).expect("config/billing.yml is invalid");
    tracing::info!(
        billing.plan.count = plans.plans().len(),
        billing.plan.free = plans.free().key(),
        "subscription plans loaded: {{billing.plan.count}} plans, free plan \
         {{billing.plan.free}}",
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

    // Realtime channels: Redis pub/sub when REDIS_URL is set, in-process
    // otherwise. Clone this into handlers and jobs that publish events.
    let channels = anubis::realtime::Channels::from_config(&config)
        .await
        .expect("REDIS_URL must name a reachable Redis");

    let app = Router::new()
        // Public profile pictures at /users/{user_id}/avatar.
        .merge(anubis::auth::avatar_router(pool.clone()))
        // The realtime channel socket at /realtime.
        .merge(anubis::realtime::router(pool.clone(), channels))
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles.clone(), &config),
        )
        // The plan an organization is on, and the Stripe redirects that change
        // it. Without STRIPE_SECRET_KEY the read still answers and the writes
        // answer 503; see docs/billing.md.
        .nest(
            "/billing",
            anubis::billing::router(pool.clone(), roles.clone(), plans.clone(), &config),
        )
        // Stripe's own events, at /webhooks/stripe-billing. Framework-mounted
        // because the subscription lifecycle is the framework's, and named so
        // that an application scaffolding its own Stripe receiver keeps
        // /webhooks/stripe for itself.
        .nest(
            "/webhooks",
            anubis::billing::webhook_router(pool.clone(), &config),
        )
        .nest(
            "/developers",
            anubis::api::management::router(pool.clone(), roles.clone()),
        )
        // Outgoing webhook subscriptions and their delivery log, beside the
        // platform applications above.
        .nest(
            "/developers",
            anubis::webhooks::router(pool.clone(), roles.clone(), &config),
        )
        // The application owns its v1 surface: the framework's endpoints, the
        // application's own, and the merged document at /openapi.json and
        // /docs, in one call.
        .nest(
            "/api/v1",
            anubis::api::v1::router_with(pool.clone(), api_v1_router(&pool, &roles), openapi()),
        )
        .nest("/account", account_router(&pool, &roles))
        // Incoming webhooks from third parties. Unauthenticated on purpose,
        // and mounted outside /account so no guard ever asks a provider for a
        // session it does not have.
        .nest("/webhooks", webhooks_router(&pool))
        // Application routes guard with TeamMember / OrganizationMember /
        // CurrentUser through these extensions.
        .layer(anubis::guard::layer(pool.clone(), roles));

    // In production the built frontend ships with the binary: every path the
    // routers above declined resolves to the SPA, so a cold load of a client
    // route works. `/account` joins the framework's own prefixes (`/webhooks`
    // among them) as a place where an unmatched path is a JSON 404 rather than
    // index.html: a caller that asked for JSON deserves an error it can act on,
    // not an HTML page and a `200`. Unset SPA_DIR leaves the browser to the
    // Vite dev server, the development default.
    let app = match &config.spa_dir {
        Some(dir) => {
            let assets = anubis::spa::Assets::new(dir)
                .expect("SPA_DIR must point at a built frontend (yarn build)")
                .reserve("/account");
            app.fallback_service(assets.into_service())
        }
        None => app,
    };

    // Background jobs run in this process, in a worker built beside this one.
    let worker = worker(&pool, plans, &config);

    let (stop_worker, worker_stops) = tokio::sync::oneshot::channel::<()>();
    let working = tokio::spawn(worker.run(async move {
        let _stopped = worker_stops.await;
    }));

    // The framework owns everything from here: the probes, the middleware, the
    // accept loop, and draining in-flight requests on SIGTERM or ctrl-c.
    server::serve(app, pool, &config)
        .await
        .expect("the server terminated unexpectedly");

    // The worker keeps running until the server has drained, because a request
    // still finishing may yet enqueue work. Then it drains its own in-flight
    // jobs, so a deploy never kills a job mid-flight.
    let _listening = stop_worker.send(());
    working
        .await
        .expect("the job worker must shut down cleanly");
}

/// Builds the background job worker this process runs.
///
/// Registering a job is the whole of the wiring: it subscribes the worker to
/// that job's queue and captures whatever the handler needs. Two jobs are the
/// framework's own, outgoing webhook delivery and the Stripe subscription
/// lifecycle; the application's are registered by `register_jobs`, which is
/// where a scaffolded job lands.
fn worker(pool: &anubis::db::DbPool, plans: PlanSet, config: &AppConfig) -> anubis::jobs::Worker {
    let deliverer = anubis::webhooks::Deliverer::new(pool.clone(), config);
    let reconciler = anubis::billing::Reconciler::new(pool.clone(), plans, config);

    let worker = anubis::jobs::Worker::builder(pool.clone())
        .register(move |job: anubis::webhooks::DeliverWebhook| {
            let deliverer = deliverer.clone();
            async move { deliverer.deliver(job).await }
        })
        // Stripe's events become subscription rows here, on their own queue.
        .register(move |job: anubis::billing::ProcessStripeEvent| {
            let reconciler = reconciler.clone();
            async move { reconciler.process(job).await }
        });

    register_jobs(pool, worker).build()
}
