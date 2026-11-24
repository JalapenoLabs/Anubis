//! Sessions behave the same in Postgres and in Redis: one narrative, run once
//! against each store.
//!
//! The narrative walks every session act a person performs or has performed on
//! them: signing in, reading the session list on the security page, signing
//! one session out from that list, signing this browser out, changing the
//! password (which signs out the rest), a session reaching its end, a password
//! reset and an operator's temporary password (each signing out everywhere),
//! and deleting the account. Every step asserts what a browser sees. The store
//! is also read directly, at the points that prove a session lives where
//! `SESSION_STORE` says, because Postgres is the default: a Redis store that
//! was never wired in would otherwise pass as Postgres.
//!
//! The deployment goes through `anubis::server::harden`, as a served
//! application does, so the configuration chooses the store exactly as it does
//! in production, and the routes outside `/auth` read the same store as the
//! routes inside it. Both runs need `DATABASE_URL`, and the Redis run needs
//! `REDIS_URL` too; each logs a skip and passes without what it needs. CI
//! provides both. Keys in the shared Redis stay apart because every account is
//! new, and the narrative never flushes or reconfigures that Redis.

mod support;

use anubis::audit;
use anubis::auth::CurrentUser;
use anubis::config::AppConfig;
use anubis::db::DbPool;
use anubis::guard::PlatformMember;
use anubis::http::ApiError;
use anubis::mail::{EmailKind, TestOutbox};
use anubis::platform::{self, Accounts};
use anubis::roles::{Action, RoleSet};
use anubis::schema::{audit_events, sessions, users};
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use redis::AsyncTypedCommands;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{PASSWORD, TestDatabase, send, session_token};
use uuid::Uuid;

/// An operator may manage accounts; nobody else operates.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  admin:
    includes: [default]
    models: {}
  operator:
    scopes: [platform]
    models:
      Account: [manage]
";

/// The seeded operator's password.
const OPERATOR_PASSWORD: &str = "the operator's own password";

/// The password the account changes to, and then resets to.
const CHANGED_PASSWORD: &str = "a second passphrase entirely";
const RESET_PASSWORD: &str = "a third passphrase, from the inbox";
const CHOSEN_PASSWORD: &str = "a fourth passphrase, chosen at last";

/// How long a session lives, as the framework promises it.
const SESSION_LIFETIME: Duration = Duration::days(anubis::auth::SESSION_TTL_DAYS);

/// The Redis key layout, restated here so a change to it fails this suite
/// instead of passing silently. See `anubis::auth::session`.
const TOKEN_KEY_PREFIX: &str = "anubis:sessions:token:";
const USER_KEY_PREFIX: &str = "anubis:sessions:user:";

#[tokio::test]
async fn sessions_behave_the_same_in_postgres() {
    let Some(database) = TestDatabase::create("session_store_postgres").await else {
        return;
    };
    narrative(&database, Store::Postgres).await;
}

#[tokio::test]
async fn sessions_behave_the_same_in_redis() {
    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        eprintln!("skipping session_store_flow Redis test: REDIS_URL is not set");
        return;
    };
    let Some(database) = TestDatabase::create("session_store_redis").await else {
        return;
    };
    let client = redis::Client::open(redis_url.as_str()).expect("REDIS_URL must parse");
    let connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("REDIS_URL must name a reachable Redis");
    narrative(
        &database,
        Store::Redis {
            url: redis_url,
            connection,
        },
    )
    .await;
}

/// The store a run configures, and the direct reads that prove it is used.
enum Store {
    Postgres,
    Redis {
        url: String,
        connection: redis::aio::MultiplexedConnection,
    },
}

impl Store {
    /// The `SESSION_STORE` and `REDIS_URL` this run's configuration carries.
    fn variables(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Postgres => vec![("SESSION_STORE", "postgres".to_owned())],
            Self::Redis { url, .. } => vec![
                ("SESSION_STORE", "redis".to_owned()),
                ("REDIS_URL", url.clone()),
            ],
        }
    }

    /// Asserts the session behind `token` is held by this store and only this
    /// one, and that it ends one session lifetime from now.
    async fn assert_holds(&self, pool: &DbPool, user_id: Uuid, token: &str) {
        let token_hash = hash(token);
        let rows = session_rows(pool, user_id).await;
        match self {
            Self::Postgres => {
                let expires_at = rows
                    .iter()
                    .find(|(stored_hash, _expires_at)| *stored_hash == token_hash)
                    .map(|(_stored_hash, expires_at)| *expires_at)
                    .expect("the session must be a row in the sessions table");
                assert_ends_one_lifetime_from_now(expires_at);
            }
            Self::Redis { connection, .. } => {
                assert!(
                    rows.is_empty(),
                    "a Redis store must write nothing to the sessions table: {rows:?}",
                );
                let mut connection = connection.clone();
                let stored = connection
                    .get(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
                    .await
                    .expect("Redis must answer")
                    .expect("the session must be a key in Redis");
                let session: Value = serde_json::from_str(&stored).expect("a session is JSON");
                assert_eq!(session["user_id"], json!(user_id), "stored: {session}");
                assert!(
                    session.get("token").is_none(),
                    "the token itself is never stored: {session}",
                );
                let expires_at: DateTime<Utc> =
                    serde_json::from_value(session["expires_at"].clone())
                        .expect("the end is a timestamp");
                assert_ends_one_lifetime_from_now(expires_at);

                // Redis drops the key on its own when the session ends.
                let seconds_left = connection
                    .ttl(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
                    .await
                    .expect("Redis must answer")
                    .raw();
                let lifetime_less_an_hour = (SESSION_LIFETIME - Duration::hours(1)).num_seconds();
                assert!(
                    i64::try_from(seconds_left)
                        .is_ok_and(|seconds| seconds > lifetime_less_an_hour),
                    "the key must expire with the session: {seconds_left}s left",
                );

                let indexed = connection
                    .zrange(format!("{USER_KEY_PREFIX}{user_id}"), 0, -1)
                    .await
                    .expect("Redis must answer");
                assert!(
                    indexed.contains(&token_hash),
                    "the user's index must name the session: {indexed:?}",
                );
            }
        }
    }

    /// Asserts the account holds no session in this store at all.
    async fn assert_holds_none(&self, pool: &DbPool, user_id: Uuid) {
        let rows = session_rows(pool, user_id).await;
        assert!(rows.is_empty(), "no session rows may remain: {rows:?}");

        if let Self::Redis { connection, .. } = self {
            let mut connection = connection.clone();
            let indexed = connection
                .zrange(format!("{USER_KEY_PREFIX}{user_id}"), 0, -1)
                .await
                .expect("Redis must answer");
            assert!(indexed.is_empty(), "the index must be empty: {indexed:?}");
        }
    }

    /// Asserts the session behind `token` is gone from this store.
    async fn assert_gone(&self, pool: &DbPool, user_id: Uuid, token: &str) {
        let token_hash = hash(token);
        let rows = session_rows(pool, user_id).await;
        assert!(
            rows.iter()
                .all(|(stored_hash, _expires_at)| *stored_hash != token_hash),
            "the row must be gone: {rows:?}",
        );

        if let Self::Redis { connection, .. } = self {
            let mut connection = connection.clone();
            let exists = connection
                .exists(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
                .await
                .expect("Redis must answer");
            assert!(!exists, "the session's key must be gone");
            let indexed = connection
                .zrange(format!("{USER_KEY_PREFIX}{user_id}"), 0, -1)
                .await
                .expect("Redis must answer");
            assert!(
                !indexed.contains(&token_hash),
                "the index must not name it: {indexed:?}",
            );
        }
    }

    /// Ends the session behind `token` a second ago, the way time would.
    ///
    /// In Redis the key is left alive and only its recorded end moves, so the
    /// store is proven to check the end itself rather than trusting Redis to
    /// have dropped the key.
    async fn expire(&self, pool: &DbPool, token: &str) {
        let token_hash = hash(token);
        let ended = Utc::now() - Duration::seconds(1);
        match self {
            Self::Postgres => {
                let mut connection = pool.get().await.expect("a connection");
                let updated =
                    diesel::update(sessions::table.filter(sessions::token_hash.eq(&token_hash)))
                        .set(sessions::expires_at.eq(ended))
                        .execute(&mut connection)
                        .await
                        .expect("the row must update");
                assert_eq!(updated, 1);
            }
            Self::Redis { connection, .. } => {
                let mut connection = connection.clone();
                let key = format!("{TOKEN_KEY_PREFIX}{token_hash}");
                let stored = connection
                    .get(&key)
                    .await
                    .expect("Redis must answer")
                    .expect("the session must exist");
                let mut session: Value = serde_json::from_str(&stored).expect("a session is JSON");
                session["expires_at"] = json!(ended);
                redis::cmd("SET")
                    .arg(&key)
                    .arg(session.to_string())
                    .arg("KEEPTTL")
                    .exec_async(&mut connection)
                    .await
                    .expect("the session must be rewritten");
            }
        }
    }
}

/// The token hash a store keys a session by.
fn hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

/// Asserts `expires_at` is one session lifetime from now, give or take a minute.
fn assert_ends_one_lifetime_from_now(expires_at: DateTime<Utc>) {
    let drift = expires_at - (Utc::now() + SESSION_LIFETIME);
    assert!(
        drift.num_seconds().abs() < 60,
        "a session ends one lifetime after it starts: ends {expires_at}",
    );
}

/// The account's rows in the `sessions` table: each token hash and its end.
async fn session_rows(pool: &DbPool, user_id: Uuid) -> Vec<(String, DateTime<Utc>)> {
    let mut connection = pool.get().await.expect("a connection");
    sessions::table
        .filter(sessions::user_id.eq(user_id))
        .select((sessions::token_hash, sessions::expires_at))
        .load(&mut connection)
        .await
        .expect("the sessions table must be readable")
}

/// A route outside `/auth`, so the store is proven to reach every router.
async fn whoami(CurrentUser(user): CurrentUser) -> impl IntoResponse {
    Json(json!({ "id": user.id, "email": user.email }))
}

/// The operator route an application writes over the framework's primitive.
async fn temporary_password(
    operator: PlatformMember,
    context: audit::Context,
    Extension(accounts): Extension<Accounts>,
    Extension(pool): Extension<DbPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Update, "Account")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let password = accounts
        .set_temporary_password(&mut connection, &operator, &context, user_id)
        .await?;
    Ok(Json(json!({ "password": password.reveal() })))
}

/// A hardened deployment over one database, with the store `store` chooses.
struct Deployment {
    router: Router,
    pool: DbPool,
    outbox: TestOutbox,
    operator: String,
}

impl Deployment {
    async fn boot(database: &TestDatabase, store: &Store) -> Self {
        let pool = database.pool().await;
        let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
        let operator_email = format!("ops-{}@example.com", Uuid::new_v4());

        let mut variables = store.variables();
        variables.extend([
            ("ANUBIS_ENV", "test".to_owned()),
            ("RATE_LIMIT_DISABLED", "true".to_owned()),
            ("ANUBIS_INITIAL_ADMIN_EMAIL", operator_email.clone()),
            (
                "ANUBIS_INITIAL_ADMIN_PASSWORD",
                OPERATOR_PASSWORD.to_owned(),
            ),
        ]);
        let config = AppConfig::from_lookup(move |name| {
            variables
                .iter()
                .find(|(variable, _value)| *variable == name)
                .map(|(_variable, value)| value.clone())
        })
        .expect("the test config must parse");
        platform::ensure_initial_admin(&pool, &config, &roles)
            .await
            .expect("the seed must appoint the operator");

        let (mailer, outbox) = anubis::mail::Mailer::test();
        let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
        let accounts = Accounts::new(&config, mailer.clone());

        let app = Router::new()
            .nest(
                "/auth",
                anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
            )
            .route("/whoami", get(whoami))
            .route(
                "/operator/accounts/{user_id}/temporary-password",
                post(temporary_password),
            )
            .layer(Extension(accounts))
            .layer(anubis::guard::layer(pool.clone(), roles));
        let router = anubis::server::harden(app, pool.clone(), &config);

        let operator = sign_in(&router, &operator_email, OPERATOR_PASSWORD).await;

        Self {
            router,
            pool,
            outbox,
            operator,
        }
    }

    /// The id of the account registered as `email`.
    async fn user_id(&self, email: &str) -> Uuid {
        let mut connection = self.pool.get().await.expect("a connection");
        users::table
            .filter(users::email.eq(email))
            .select(users::id)
            .first(&mut connection)
            .await
            .expect("the account exists")
    }

    /// Who `token` signs in as, or the status that says nobody.
    async fn whoami(&self, token: &str) -> StatusCode {
        let (status, _headers, _body) =
            send(&self.router, "GET", "/whoami", None, Some(token)).await;
        status
    }

    /// The session list the security page shows the holder of `token`.
    async fn listed(&self, token: &str) -> Vec<Value> {
        let (status, _headers, body) =
            send(&self.router, "GET", "/auth/sessions", None, Some(token)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        body["sessions"]
            .as_array()
            .expect("the list is an array")
            .clone()
    }

    /// The audit rows recorded about `subject_id` with `action`.
    async fn audited(&self, subject_id: Uuid, action: &str) -> Vec<audit::AuditEvent> {
        let mut connection = self.pool.get().await.expect("a connection");
        audit_events::table
            .filter(audit_events::subject_id.eq(subject_id))
            .filter(audit_events::action.eq(action))
            .select(audit::AuditEvent::as_select())
            .load(&mut connection)
            .await
            .expect("the audit log must be readable")
    }

    /// The token in the most recent password reset email to `email`.
    fn reset_token(&self, email: &str) -> String {
        let reset = self
            .outbox
            .emails()
            .into_iter()
            .rev()
            .find(|sent| sent.to == email && sent.kind == EmailKind::ResetPassword)
            .expect("a reset email must have been sent");
        let link = reset
            .params
            .get("link")
            .expect("the reset email must carry its link");
        link.split_once("token=")
            .expect("the link must carry a token")
            .1
            .to_owned()
    }
}

/// Signs in with a password and returns the session token.
async fn sign_in(router: &Router, email: &str, password: &str) -> String {
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": email, "password": password })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers)
}

/// The id of the listed session at `index`.
fn listed_id(listed: &[Value], index: usize) -> String {
    listed[index]["id"]
        .as_str()
        .expect("a session id is a string")
        .to_owned()
}

#[expect(
    clippy::too_many_lines,
    reason = "one linear narrative, run against each store; splitting it would hide the order \
              the acts happen in"
)]
async fn narrative(database: &TestDatabase, store: Store) {
    let deployment = Deployment::boot(database, &store).await;
    let router = &deployment.router;
    let email = format!("sessions-{}@example.com", Uuid::new_v4());

    // ---- Registering signs in, and the session lives in the chosen store.
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/register",
        Some(&json!({ "email": email, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let first = session_token(&headers);
    let user_id = deployment.user_id(&email).await;
    assert_eq!(deployment.whoami(&first).await, StatusCode::OK);
    store.assert_holds(&deployment.pool, user_id, &first).await;

    // A token nobody issued signs in as nobody, never as an error.
    assert_eq!(
        deployment.whoami("not-a-session-anybody-issued").await,
        StatusCode::UNAUTHORIZED,
    );

    // ---- Signing in again adds sessions; the list shows them oldest first.
    let second = sign_in(router, &email, PASSWORD).await;
    let third = sign_in(router, &email, PASSWORD).await;
    store.assert_holds(&deployment.pool, user_id, &third).await;

    let listed = deployment.listed(&first).await;
    assert_eq!(listed.len(), 3, "listed: {listed:?}");
    let created: Vec<DateTime<Utc>> = listed
        .iter()
        .map(|session| serde_json::from_value(session["created_at"].clone()).expect("a timestamp"))
        .collect();
    assert!(created.is_sorted(), "the list is oldest first: {listed:?}");
    let current: Vec<&Value> = listed
        .iter()
        .filter(|session| session["current"] == json!(true))
        .collect();
    assert_eq!(current.len(), 1, "exactly one is this browser: {listed:?}");
    assert_eq!(
        current[0]["id"], listed[0]["id"],
        "the first session asked, so it is the current one",
    );
    let newest_id = listed_id(&listed, 2);

    // ---- Signing one session out from the list ends that one alone.
    let (status, _headers, body) = send(
        router,
        "DELETE",
        &format!("/auth/sessions/{newest_id}"),
        None,
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(deployment.whoami(&third).await, StatusCode::UNAUTHORIZED);
    assert_eq!(deployment.whoami(&first).await, StatusCode::OK);
    store.assert_gone(&deployment.pool, user_id, &third).await;
    let revoked_id: Uuid = newest_id.parse().expect("a session id is a UUID");
    assert_eq!(
        deployment
            .audited(revoked_id, audit::SESSION_REVOKED)
            .await
            .len(),
        1,
        "signing a session out is recorded once, against that session",
    );

    // Again, and with an id nobody holds: both are a 404.
    for missing in [newest_id.clone(), Uuid::new_v4().to_string()] {
        let (status, _headers, _body) = send(
            router,
            "DELETE",
            &format!("/auth/sessions/{missing}"),
            None,
            Some(&first),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "for {missing}");
    }

    // Another account's session id answers the same 404 and ends nothing.
    let stranger_email = format!("stranger-{}@example.com", Uuid::new_v4());
    let stranger = support::register(router, &stranger_email).await;
    let stranger_listed = deployment.listed(&stranger).await;
    let stranger_id = listed_id(&stranger_listed, 0);
    let (status, _headers, _body) = send(
        router,
        "DELETE",
        &format!("/auth/sessions/{stranger_id}"),
        None,
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(deployment.whoami(&stranger).await, StatusCode::OK);

    // ---- Signing this browser out ends its session, and is safe to repeat.
    for attempt in 0..2 {
        let (status, _headers, _body) =
            send(router, "POST", "/auth/logout", None, Some(&second)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "sign-out {attempt}");
    }
    assert_eq!(deployment.whoami(&second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(deployment.whoami(&first).await, StatusCode::OK);
    store.assert_gone(&deployment.pool, user_id, &second).await;
    assert_eq!(deployment.listed(&first).await.len(), 1);

    // ---- Changing the password keeps this browser and signs out the rest.
    let elsewhere = sign_in(router, &email, PASSWORD).await;
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/change-password",
        Some(&json!({ "current_password": PASSWORD, "new_password": CHANGED_PASSWORD })),
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(deployment.whoami(&first).await, StatusCode::OK);
    assert_eq!(
        deployment.whoami(&elsewhere).await,
        StatusCode::UNAUTHORIZED
    );
    store
        .assert_gone(&deployment.pool, user_id, &elsewhere)
        .await;
    store.assert_holds(&deployment.pool, user_id, &first).await;

    // ---- A session past its end lets nobody in and leaves the list.
    let ending = sign_in(router, &email, CHANGED_PASSWORD).await;
    assert_eq!(deployment.listed(&first).await.len(), 2);
    store.expire(&deployment.pool, &ending).await;
    assert_eq!(deployment.whoami(&ending).await, StatusCode::UNAUTHORIZED);
    let listed = deployment.listed(&first).await;
    assert_eq!(
        listed.len(),
        1,
        "an ended session is not listed: {listed:?}"
    );
    assert_eq!(listed[0]["current"], json!(true));

    // ---- A password reset signs out everywhere, this browser included.
    let (status, _headers, _body) = send(
        router,
        "POST",
        "/auth/password-reset/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let reset = deployment.reset_token(&email);
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/password-reset/confirm",
        Some(&json!({ "token": reset, "password": RESET_PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(deployment.whoami(&first).await, StatusCode::UNAUTHORIZED);
    store.assert_holds_none(&deployment.pool, user_id).await;

    // ---- An operator's temporary password signs out everywhere too.
    let rescued = sign_in(router, &email, RESET_PASSWORD).await;
    let also_rescued = sign_in(router, &email, RESET_PASSWORD).await;
    let (status, _headers, body) = send(
        router,
        "POST",
        &format!("/operator/accounts/{user_id}/temporary-password"),
        None,
        Some(&deployment.operator),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let temporary = body["password"]
        .as_str()
        .expect("the password is handed over")
        .to_owned();
    for token in [&rescued, &also_rescued] {
        assert_eq!(deployment.whoami(token).await, StatusCode::UNAUTHORIZED);
    }
    store.assert_holds_none(&deployment.pool, user_id).await;
    // The operator's own session is untouched by what it did to another's.
    assert_eq!(
        deployment.whoami(&deployment.operator).await,
        StatusCode::OK
    );

    // ---- Deleting the account ends every session it still has.
    let last = sign_in(router, &email, &temporary).await;
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/change-password",
        Some(&json!({ "current_password": temporary, "new_password": CHOSEN_PASSWORD })),
        Some(&last),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let other_device = sign_in(router, &email, CHOSEN_PASSWORD).await;
    store
        .assert_holds(&deployment.pool, user_id, &other_device)
        .await;

    // Every sign-in was recorded, by whichever path, before the account goes.
    let sign_ins = deployment.audited(user_id, audit::SESSION_CREATED).await;
    assert_eq!(
        sign_ins.len(),
        9,
        "registration and eight password sign-ins, each recorded once",
    );

    let (status, _headers, body) = send(
        router,
        "DELETE",
        "/auth/account",
        Some(&json!({ "password": CHOSEN_PASSWORD })),
        Some(&last),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    for token in [&last, &other_device] {
        assert_eq!(deployment.whoami(token).await, StatusCode::UNAUTHORIZED);
    }
    store.assert_holds_none(&deployment.pool, user_id).await;
}
