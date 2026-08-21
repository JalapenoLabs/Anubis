//! The framework's routers, composed over one [`TestDatabase`].
//!
//! What an application's `main` composes, minus the application: auth,
//! tenancy, realtime, and the developer surface, behind the guard layer they
//! all read their extensions from. A suite that wants the production
//! middleware wraps the router in `anubis::server::harden` itself, so the
//! difference between a bare handler and a hardened one stays visible in the
//! test that cares about it.

use std::fmt::{self, Debug, Formatter};
use std::net::SocketAddr;

use anubis::billing::PlanSet;
use anubis::db::DbPool;
use anubis::mail::TestOutbox;
use anubis::realtime::Channels;
use anubis::roles::RoleSet;
use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use super::TestDatabase;

/// The starter's role vocabulary, which is what tenancy bootstrapping needs.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  editor:
    includes: [default]
    models: {}
  billing:
    includes: [default]
    models: {}
  admin:
    includes: [editor, billing]
    models:
      Team: [manage]
";

/// The password every account in these suites registers with.
pub const PASSWORD: &str = "correct horse battery staple";

/// A booted application: its router, its pool, and its outbox.
#[derive(Clone)]
pub struct Harness {
    /// The composed router, cloned into whatever drives it.
    pub router: Router,
    /// A pool onto the same database, for reading rows directly.
    pub pool: DbPool,
    /// Every email the application sent.
    pub outbox: TestOutbox,
    /// The realtime channels the router publishes to.
    pub channels: Channels,
}

impl Debug for Harness {
    /// Names the outbox and nothing else: the pool holds connections that do
    /// not implement `Debug`, and a router renders as noise.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("outbox", &self.outbox)
            .finish_non_exhaustive()
    }
}

impl Harness {
    /// Composes the framework's routers over `database`.
    ///
    /// # Panics
    /// Panics when the database is unreachable or the test config, which is
    /// this file's own, does not parse.
    pub async fn boot(database: &TestDatabase) -> Self {
        Self::boot_with(database, None).await
    }

    /// Composes the same routers with an application's plans in force.
    ///
    /// The difference the plans make is the `seats` limit: an invitation past
    /// it is refused. A suite that is not about limits boots without them, the
    /// way an application with no `config/billing.yml` runs.
    ///
    /// # Panics
    /// Panics for the same reasons [`Harness::boot`] does.
    pub async fn boot_with_plans(database: &TestDatabase, plans: PlanSet) -> Self {
        Self::boot_with(database, Some(plans)).await
    }

    async fn boot_with(database: &TestDatabase, plans: Option<PlanSet>) -> Self {
        let pool = database.pool().await;

        let config = Self::config();
        let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
        let (mailer, outbox) = anubis::mail::Mailer::test();
        let channels = Channels::in_process();
        let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

        let router = Router::new()
            .nest(
                "/auth",
                anubis::auth::router(pool.clone(), mailer.clone(), &config, &rate_limit),
            )
            .nest(
                "/tenancy",
                anubis::tenancy::router(
                    pool.clone(),
                    mailer,
                    roles.clone(),
                    plans,
                    &config,
                    &rate_limit,
                ),
            )
            .nest(
                "/developers",
                anubis::api::management::router(pool.clone(), roles.clone()),
            )
            .nest(
                "/developers",
                anubis::webhooks::router(pool.clone(), roles.clone(), &config),
            )
            .nest("/account", anubis::notifications::router(pool.clone()))
            .nest(
                "/account",
                anubis::audit::router(pool.clone(), roles.clone()),
            )
            .merge(anubis::realtime::router(pool.clone(), channels.clone()))
            .merge(anubis::auth::avatar_router(pool.clone()))
            .layer(anubis::guard::layer(pool.clone(), roles));

        Self {
            router,
            pool,
            outbox,
            channels,
        }
    }

    /// Serves this harness's router on a loopback port.
    ///
    /// # Panics
    /// Panics when no loopback port is available.
    pub fn serve(&self) -> SocketAddr {
        serve(self.router.clone())
    }

    /// The application config these suites boot with.
    ///
    /// `ANUBIS_ENV=test` always, and `RATE_LIMIT_DISABLED` passed through from
    /// the environment, which is how the login storm turns the limiter off
    /// without every other suite having to think about it. Nothing else is
    /// read, so a developer's own `.env` cannot change what a test proves.
    ///
    /// # Panics
    /// Panics when the config does not parse, which for this lookup is a
    /// framework bug rather than a test failure.
    pub fn config() -> anubis::config::AppConfig {
        anubis::config::AppConfig::from_lookup(|name| match name {
            "ANUBIS_ENV" => Some("test".to_owned()),
            "RATE_LIMIT_DISABLED" => std::env::var(name).ok(),
            _ => None,
        })
        .expect("the test config must parse")
    }

    /// The team registration bootstrapped for the account behind `cookie`.
    ///
    /// # Panics
    /// Panics when the account has no bootstrapped team, which registration
    /// always creates.
    pub async fn bootstrapped_team(&self, cookie: &str) -> Uuid {
        let (status, _headers, body) = send(
            &self.router,
            "GET",
            "/tenancy/memberships",
            None,
            Some(cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        body["organizations"][0]["teams"][0]["id"]
            .as_str()
            .expect("registration bootstraps one team")
            .parse()
            .expect("a team id is a UUID")
    }
}

/// Serves a router on a loopback port and returns its address.
///
/// Only a websocket and a benchmark need this: everything else drives the
/// router directly with `oneshot`, which is faster and needs no port. The
/// server runs with connect info, as production does, so the rate limiter can
/// charge a caller; it stops when the test's runtime does.
///
/// Must be called from inside a runtime, which every `#[tokio::test]` is.
///
/// # Panics
/// Panics when no loopback port is available.
pub fn serve(router: Router) -> SocketAddr {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port must be available");
    listener
        .set_nonblocking(true)
        .expect("the listener must go non-blocking");
    let address = listener.local_addr().expect("the listener must be bound");
    let listener =
        tokio::net::TcpListener::from_std(listener).expect("the listener must adopt the socket");

    tokio::spawn(async move {
        let _served = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    address
}

/// Sends one JSON request, as a signed-in user or as nobody.
///
/// # Panics
/// Panics when the request cannot be built or the router cannot answer it.
pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }

    let request = match body {
        Some(value) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body must serialize"),
            )),
        None => builder.body(Body::empty()),
    }
    .expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

/// Registers an account and returns its session token.
///
/// # Panics
/// Panics when registration is refused.
pub async fn register(router: &Router, email: &str) -> String {
    let credentials = json!({ "email": email, "password": PASSWORD });
    let (status, headers, body) =
        send(router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// Reads the token out of the invitation email sent to `email`.
///
/// Matched on the subject as well as the recipient, because an account that
/// registered before it was invited also has a verification email waiting, and
/// that one carries a `token=` link of its own.
///
/// # Panics
/// Panics when no invitation to `email` was sent, or when the one that was
/// carries no link.
pub fn invitation_token(outbox: &TestOutbox, email: &str) -> String {
    let invitation = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation
        .text_body
        .split_once("token=")
        .expect("the invitation email must carry a link");
    rest.split_whitespace()
        .next()
        .expect("the token ends at whitespace")
        .to_owned()
}

/// Reads the session cookie out of a response's `Set-Cookie` headers.
///
/// # Panics
/// Panics when the response set no session cookie.
pub fn session_token(headers: &HeaderMap) -> String {
    for value in headers.get_all(SET_COOKIE) {
        if let Some(rest) = value
            .to_str()
            .ok()
            .and_then(|rendered| rendered.strip_prefix("anubis_session="))
        {
            let token = rest.split(';').next().unwrap_or_default();
            if !token.is_empty() {
                return token.to_owned();
            }
        }
    }
    panic!("no session cookie in response");
}
