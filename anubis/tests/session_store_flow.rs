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
use diesel_async::{AsyncConnection, RunQueryDsl};
use redis::AsyncTypedCommands;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{PASSWORD, TestDatabase, send, session_token};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use url::Url;
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

#[tokio::test]
async fn a_sign_out_everywhere_ends_the_sessions_of_both_stores() {
    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        eprintln!("skipping session_store_flow cross-store test: REDIS_URL is not set");
        return;
    };
    let Some(database) = TestDatabase::create("session_store_both").await else {
        return;
    };
    let client = redis::Client::open(redis_url.as_str()).expect("REDIS_URL must parse");
    let connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let redis_store = Store::Redis {
        url: redis_url,
        connection,
    };

    // One database, served once through each store: what a deployment that
    // switched SESSION_STORE, and might switch back, leaves behind.
    let in_postgres = Deployment::boot(&database, &Store::Postgres).await;
    let in_redis = Deployment::boot(&database, &redis_store).await;
    let email = format!("switching-{}@example.com", Uuid::new_v4());
    let postgres_session = support::register(&in_postgres.router, &email).await;
    let redis_session = sign_in(&in_redis.router, &email, PASSWORD).await;
    assert_eq!(in_postgres.whoami(&postgres_session).await, StatusCode::OK);
    assert_eq!(in_redis.whoami(&redis_session).await, StatusCode::OK);

    // A reset under Redis never touches the sessions table, and the session
    // there still ends: switching back cannot revive it.
    reset_password(&in_redis, &email, RESET_PASSWORD).await;
    assert_eq!(
        in_postgres.whoami(&postgres_session).await,
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        in_redis.whoami(&redis_session).await,
        StatusCode::UNAUTHORIZED,
    );

    // And the other way round.
    let postgres_session = sign_in(&in_postgres.router, &email, RESET_PASSWORD).await;
    let redis_session = sign_in(&in_redis.router, &email, RESET_PASSWORD).await;
    reset_password(&in_postgres, &email, CHANGED_PASSWORD).await;
    assert_eq!(
        in_redis.whoami(&redis_session).await,
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        in_postgres.whoami(&postgres_session).await,
        StatusCode::UNAUTHORIZED,
    );
}

#[tokio::test]
async fn a_stamp_no_generation_was_given_stays_dead_in_postgres() {
    let Some(database) = TestDatabase::create("session_store_stamp_postgres").await else {
        return;
    };
    stamp_left_behind(&database, &Store::Postgres).await;
}

#[tokio::test]
async fn a_stamp_no_generation_was_given_stays_dead_in_redis() {
    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        eprintln!("skipping session_store_flow Redis stamp test: REDIS_URL is not set");
        return;
    };
    let Some(database) = TestDatabase::create("session_store_stamp_redis").await else {
        return;
    };
    let client = redis::Client::open(redis_url.as_str()).expect("REDIS_URL must parse");
    let connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let store = Store::Redis {
        url: redis_url,
        connection,
    };
    stamp_left_behind(&database, &store).await;
}

/// What a password change leaves behind when its re-stamp of the kept browser
/// reached the store and its commit never did: a session stamped with a
/// generation drawn for the account and never taken by it. That session is
/// out at once, and must stay out after the next sign-out-everywhere, even
/// when the store keeps it, because the next generation must not be the value
/// it carries.
async fn stamp_left_behind(database: &TestDatabase, store: &Store) {
    let deployment = Deployment::boot(database, store).await;
    let router = &deployment.router;
    let email = format!("rolled-back-{}@example.com", Uuid::new_v4());
    let session = support::register(router, &email).await;
    let user_id = deployment.user_id(&email).await;

    // Drawn from the same sequence a real password change draws from, and so
    // exactly the value a rolled-back change would have left.
    let drawn = draw_generation(&deployment.pool).await;
    store.stamp(&deployment.pool, &session, drawn).await;
    assert_eq!(
        deployment.whoami(&session).await,
        StatusCode::UNAUTHORIZED,
        "a stamp the account never took signs nobody in",
    );

    // The store loses track of the session and gets it back after the reset,
    // so nothing but the generation can keep it out.
    let leftover = store.remember(&deployment.pool, &session).await;
    store.evict_index(user_id).await;
    reset_password(&deployment, &email, RESET_PASSWORD).await;
    store.bring_back(&deployment.pool, leftover).await;
    assert_eq!(
        deployment.whoami(&session).await,
        StatusCode::UNAUTHORIZED,
        "the next sign-out-everywhere never takes a value a rolled-back change drew",
    );
}

/// A sweep runs after its commit, so it can run after a later
/// sign-out-everywhere has committed too. It must remove only the sessions its
/// own revocation ended, never a session that later one kept. Only Redis
/// sweeps after the commit; the Postgres store deletes inside the transaction.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_sweep_spares_a_session_a_later_sign_out_everywhere_kept() {
    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        eprintln!("skipping session_store_flow late sweep test: REDIS_URL is not set");
        return;
    };
    let Some(database) = TestDatabase::create("session_store_late_sweep").await else {
        return;
    };
    let gate = RedisGate::start(&redis_url).await;
    let client = redis::Client::open(redis_url.as_str()).expect("REDIS_URL must parse");
    let connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("REDIS_URL must name a reachable Redis");
    // The deployment reaches Redis through the gate; the test reads and
    // writes it directly, so the test is never held.
    let store = Store::Redis {
        url: gate.url.clone(),
        connection,
    };
    let deployment = Deployment::boot(&database, &store).await;
    let router = &deployment.router;
    let email = format!("late-sweep-{}@example.com", Uuid::new_v4());
    let ended = support::register(router, &email).await;
    let kept = sign_in(router, &email, PASSWORD).await;
    let user_id = deployment.user_id(&email).await;

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

    // A reset reaches Redis only to sweep, after its commit, so holding the
    // gate holds exactly the reset's sweep.
    let mut connection = deployment.pool.get().await.expect("a connection");
    let before: i64 = users::table
        .find(user_id)
        .select(users::session_generation)
        .first(&mut connection)
        .await
        .expect("the account exists");
    gate.hold();
    let resetting = {
        let router = router.clone();
        tokio::spawn(async move {
            let (status, _headers, _body) = send(
                &router,
                "POST",
                "/auth/password-reset/confirm",
                Some(&json!({ "token": reset, "password": RESET_PASSWORD })),
                None,
            )
            .await;
            status
        })
    };
    wait_for_the_generation_to_leave(&mut connection, user_id, before).await;

    // What a later password change commits while the reset's sweep waits: a
    // newer generation, and the browser it keeps re-stamped with it.
    let later: i64 = diesel::update(users::table.find(user_id))
        .set(users::session_generation.eq(nextval("session_generations")))
        .returning(users::session_generation)
        .get_result(&mut connection)
        .await
        .expect("the account must move");
    store.stamp(&deployment.pool, &kept, later).await;
    gate.release();

    let status = resetting.await.expect("the reset must not panic");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        deployment.whoami(&kept).await,
        StatusCode::OK,
        "a late sweep spares a session a later sign-out-everywhere kept",
    );
    // Proves the sweep ran after the release rather than giving up while
    // held, which would leave `kept` alone for the wrong reason.
    store.assert_gone(&deployment.pool, user_id, &ended).await;
}

/// Every sign-in records `session.created` before it creates the session, and
/// that insert's foreign key holds `FOR KEY SHARE` on the account's row until
/// the sign-in commits. A sign-out-everywhere must never wait on that lock: the
/// sign-in goes on to ask for `FOR SHARE`, which waits on the sign-out, and the
/// two would deadlock, failing one of them with `500`. The store does not
/// matter, since the generation moves in Postgres for both.
#[tokio::test(flavor = "multi_thread")]
async fn a_reset_never_waits_on_a_sign_in_that_has_recorded_itself() {
    let Some(database) = TestDatabase::create("session_store_key_share").await else {
        return;
    };
    let deployment = Deployment::boot(&database, &Store::Postgres).await;
    let email = format!("key-share-{}@example.com", Uuid::new_v4());
    support::register(&deployment.router, &email).await;
    let user_id = deployment.user_id(&email).await;
    let (status, _headers, _body) = send(
        &deployment.router,
        "POST",
        "/auth/password-reset/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let reset = deployment.reset_token(&email);

    let mut signing_in = deployment.pool.get().await.expect("a connection");
    let status = signing_in
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            // The lock a sign-in's audit insert holds from here to its commit.
            users::table
                .find(user_id)
                .select(users::id)
                .for_key_share()
                .first::<Uuid>(transaction)
                .await?;

            let body = json!({ "token": reset, "password": RESET_PASSWORD });
            let confirm = send(
                &deployment.router,
                "POST",
                "/auth/password-reset/confirm",
                Some(&body),
                None,
            );
            let (status, _headers, _body) =
                tokio::time::timeout(std::time::Duration::from_secs(10), confirm)
                    .await
                    .expect("a reset must not wait on the lock a sign-in in flight holds");
            Ok(status)
        })
        .await
        .expect("holding the account must succeed");
    assert_eq!(status, StatusCode::OK);
}

/// Waits until the account's generation is no longer `before`, which is how
/// the late sweep test knows the reset it started has committed.
async fn wait_for_the_generation_to_leave(
    connection: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    before: i64,
) {
    for _attempt in 0..1000 {
        let current: i64 = users::table
            .find(user_id)
            .select(users::session_generation)
            .first(connection)
            .await
            .expect("the account exists");
        if current != before {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the reset never committed");
}

/// A relay in front of Redis that can hold what a deployment sends, so a test
/// decides when that deployment's next Redis command arrives.
///
/// Holding is only ever brief: the deployment gives up on a command Redis has
/// not answered within a second, so a test releases well inside that.
struct RedisGate {
    /// The `REDIS_URL` that reaches Redis through the gate.
    url: String,
    open: watch::Sender<bool>,
}

impl RedisGate {
    async fn start(redis_url: &str) -> Self {
        let mut url = Url::parse(redis_url).expect("REDIS_URL must parse");
        let redis_addresses = url
            .socket_addrs(|| Some(6379))
            .expect("REDIS_URL must name a host");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port must be free");
        let address = listener.local_addr().expect("the port is bound");
        url.set_host(Some("127.0.0.1"))
            .expect("a Redis URL takes a host");
        url.set_port(Some(address.port()))
            .expect("a Redis URL takes a port");

        let (open, gate) = watch::channel(true);
        tokio::spawn(async move {
            while let Ok((client, _peer)) = listener.accept().await {
                let server = TcpStream::connect(redis_addresses.as_slice())
                    .await
                    .expect("Redis must accept a connection");
                tokio::spawn(relay(client, server, gate.clone()));
            }
        });
        Self {
            url: url.to_string(),
            open,
        }
    }

    fn hold(&self) {
        self.open.send_replace(false);
    }

    fn release(&self) {
        self.open.send_replace(true);
    }
}

/// Carries one connection's bytes both ways, holding what the client sends
/// while the gate is closed. The relay ends when either side closes or the
/// gate is dropped with its test.
async fn relay(client: TcpStream, server: TcpStream, mut gate: watch::Receiver<bool>) {
    let (mut from_client, mut to_client) = client.into_split();
    let (mut from_server, mut to_server) = server.into_split();
    tokio::spawn(async move { tokio::io::copy(&mut from_server, &mut to_client).await });

    let mut buffer = vec![0_u8; 16 * 1024];
    while let Ok(read) = from_client.read(&mut buffer).await {
        let Some(sent) = buffer.get(..read).filter(|sent| !sent.is_empty()) else {
            return;
        };
        if gate.wait_for(|open| *open).await.is_err() || to_server.write_all(sent).await.is_err() {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_change_racing_a_reset_never_overwrites_it() {
    let Some(database) = TestDatabase::create("session_store_change_race").await else {
        return;
    };
    let deployment = Deployment::boot(&database, &Store::Postgres).await;
    let email = format!("racing-{}@example.com", Uuid::new_v4());
    let session = support::register(&deployment.router, &email).await;
    let user_id = deployment.user_id(&email).await;

    // The test holds the account's row, lets the change get past its session
    // check and its argon2 runs to the update it makes, and only then commits
    // what a reset commits: a new generation and a new password. Without the
    // hold, the scheduler usually finishes one request before the other
    // starts, and the race this test is named for never runs.
    let mut blocker = deployment.pool.get().await.expect("a connection");
    let changing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            users::table
                .find(user_id)
                .select(users::id)
                .for_update()
                .first::<Uuid>(transaction)
                .await?;

            let router = deployment.router.clone();
            let change = tokio::spawn(async move {
                let (status, _headers, _body) = send(
                    &router,
                    "POST",
                    "/auth/change-password",
                    Some(&json!({
                        "current_password": PASSWORD,
                        "new_password": CHANGED_PASSWORD,
                    })),
                    Some(&session),
                )
                .await;
                status
            });
            wait_for_lock_waits(&deployment.pool, 1).await;

            diesel::update(users::table.find(user_id))
                .set((
                    users::session_generation.eq(nextval("session_generations")),
                    users::password_hash.eq(RESET_HASH_MARKER),
                ))
                .execute(transaction)
                .await?;
            Ok(change)
        })
        .await
        .expect("holding the account must succeed");
    drop(blocker);

    let status = changing.await.expect("the change must not panic");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a change checked under the old generation is refused",
    );
    let mut connection = deployment.pool.get().await.expect("a connection");
    let password_hash: String = users::table
        .find(user_id)
        .select(users::password_hash)
        .first(&mut connection)
        .await
        .expect("the account exists");
    assert_eq!(
        password_hash, RESET_HASH_MARKER,
        "the reset's password survives the change that raced it",
    );
}

/// Two changes sent from one browser, a double submit or a second tab, are
/// both checked against the same generation. The first commits and keeps this
/// browser signed in; the second is refused, since its current password was
/// checked against a hash the first replaced, but the browser is still signed
/// in, so it must not be told otherwise.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_change_from_the_same_browser_keeps_it_signed_in() {
    let Some(database) = TestDatabase::create("session_store_double_change").await else {
        return;
    };
    let deployment = Deployment::boot(&database, &Store::Postgres).await;
    let email = format!("double-{}@example.com", Uuid::new_v4());
    let session = support::register(&deployment.router, &email).await;
    let user_id = deployment.user_id(&email).await;

    // Both changes get past their session check and argon2 to the update,
    // where the held row queues them, so both were checked under the same
    // generation.
    let mut blocker = deployment.pool.get().await.expect("a connection");
    let changes = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            users::table
                .find(user_id)
                .select(users::id)
                .for_update()
                .first::<Uuid>(transaction)
                .await?;
            let changes: Vec<_> = (0..2)
                .map(|_change| {
                    let router = deployment.router.clone();
                    let session = session.clone();
                    tokio::spawn(async move {
                        let (status, _headers, _body) = send(
                            &router,
                            "POST",
                            "/auth/change-password",
                            Some(&json!({
                                "current_password": PASSWORD,
                                "new_password": CHANGED_PASSWORD,
                            })),
                            Some(&session),
                        )
                        .await;
                        status
                    })
                })
                .collect();
            wait_for_lock_waits(&deployment.pool, 2).await;
            Ok(changes)
        })
        .await
        .expect("holding the account must succeed");
    drop(blocker);

    let mut statuses = Vec::with_capacity(changes.len());
    for change in changes {
        statuses.push(change.await.expect("the change must not panic"));
    }
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "one change commits and the other is refused as a conflict",
    );
    assert_eq!(
        deployment.whoami(&session).await,
        StatusCode::OK,
        "the browser both changes came from is still signed in",
    );
}

/// A password sign-in reads the account's generation with its credentials,
/// then spends an argon2 check before it writes the session. A reset that
/// commits in that gap must end the sign-in too: creating the session reads
/// the generation again under `FOR SHARE`, waits for the reset, sees it moved,
/// and refuses with `401` instead of issuing a session.
#[tokio::test(flavor = "multi_thread")]
async fn a_password_sign_in_racing_a_reset_is_refused() {
    let Some(database) = TestDatabase::create("session_store_sign_in_race").await else {
        return;
    };
    let deployment = Deployment::boot(&database, &Store::Postgres).await;
    let email = format!("sign-in-race-{}@example.com", Uuid::new_v4());
    support::register(&deployment.router, &email).await;
    let user_id = deployment.user_id(&email).await;

    // The reset's new generation is written and left uncommitted while the
    // sign-in reads the old one with its credentials and reaches `create`,
    // whose `FOR SHARE` waits on it. Only then does the reset commit.
    let mut resetting = deployment.pool.get().await.expect("a connection");
    let signing_in = resetting
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            diesel::update(users::table.find(user_id))
                .set(users::session_generation.eq(nextval("session_generations")))
                .execute(transaction)
                .await?;
            let router = deployment.router.clone();
            let email = email.clone();
            let signing_in = tokio::spawn(async move {
                send(
                    &router,
                    "POST",
                    "/auth/login",
                    Some(&json!({ "email": email, "password": PASSWORD })),
                    None,
                )
                .await
            });
            wait_for_lock_waits(&deployment.pool, 1).await;
            Ok(signing_in)
        })
        .await
        .expect("the reset must commit");
    drop(resetting);

    let (status, headers, body) = signing_in.await.expect("the sign-in must not panic");
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert!(
        headers.get(axum::http::header::SET_COOKIE).is_none(),
        "a refused sign-in sets no cookie: {headers:?}",
    );
    assert_eq!(
        session_rows(&deployment.pool, user_id).await.len(),
        1,
        "only the registration's session exists",
    );
}

/// What the racing reset sets the password hash to, so the test can tell
/// whether the change overwrote it. Never a hash anything verifies against.
const RESET_HASH_MARKER: &str = "set-by-the-racing-reset";

/// Waits until `expected` backends in this test's database are waiting on a
/// lock, which is how a race test knows its racers reached the row it holds.
async fn wait_for_lock_waits(pool: &DbPool, expected: i64) {
    #[derive(QueryableByName)]
    struct Waiting {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        waiting: i64,
    }

    let mut connection = pool.get().await.expect("a connection");
    for _attempt in 0..600 {
        let waiting: Waiting = diesel::sql_query(
            "SELECT count(*) AS waiting FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .get_result(&mut connection)
        .await
        .expect("pg_stat_activity must be readable");
        if waiting.waiting >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the racing request never reached the account's row");
}

#[tokio::test]
async fn a_second_factor_answering_a_challenge_from_before_a_reset_signs_nobody_in() {
    let Some(database) = TestDatabase::create("session_store_challenge").await else {
        return;
    };
    let deployment = Deployment::boot(&database, &Store::Postgres).await;
    let router = &deployment.router;
    let email = format!("two-factor-{}@example.com", Uuid::new_v4());
    let cookie = support::register(router, &email).await;

    let (status, _headers, body) =
        send(router, "POST", "/auth/mfa/totp/setup", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let secret = body["secret"].as_str().expect("a secret").to_owned();
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/mfa/totp/confirm",
        Some(&json!({ "code": totp_now(&secret) })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // The password is checked and a challenge issued; then the account is
    // reset before the second factor answers.
    let stale_challenge = challenge(router, &email, PASSWORD).await;
    reset_password(&deployment, &email, RESET_PASSWORD).await;
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/mfa/verify",
        Some(&json!({ "mfa_token": stale_challenge, "code": totp_now(&secret) })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert!(
        headers.get(axum::http::header::SET_COOKIE).is_none(),
        "no session is issued for a challenge the reset ended",
    );

    // The new password and the same second factor still sign in.
    let fresh_challenge = challenge(router, &email, RESET_PASSWORD).await;
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/mfa/verify",
        Some(&json!({ "mfa_token": fresh_challenge, "code": totp_now(&secret) })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers);
}

/// Signs in with a password on an account with a second factor, answering
/// the challenge token the second factor must complete.
async fn challenge(router: &Router, email: &str, password: &str) -> String {
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": email, "password": password })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["mfa_token"]
        .as_str()
        .expect("a second factor is asked for")
        .to_owned()
}

/// The TOTP code for `secret` right now.
fn totp_now(secret: &str) -> String {
    let now = u64::try_from(Utc::now().timestamp()).expect("a sane clock");
    anubis::auth::totp::code_at(secret, now).expect("the secret decodes")
}

diesel::define_sql_function! {
    /// Postgres's `nextval`, for drawing from the generation sequence.
    fn nextval(sequence: diesel::sql_types::Text) -> diesel::sql_types::BigInt;
}

/// Draws the next value of the sequence session generations come from.
async fn draw_generation(pool: &DbPool) -> i64 {
    let mut connection = pool.get().await.expect("a connection");
    diesel::select(nextval("session_generations"))
        .get_result(&mut connection)
        .await
        .expect("the sequence must draw")
}

/// Resets `email`'s password to `password` through `deployment`'s routes.
async fn reset_password(deployment: &Deployment, email: &str, password: &str) {
    let (status, _headers, _body) = send(
        &deployment.router,
        "POST",
        "/auth/password-reset/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let reset = deployment.reset_token(email);
    let (status, _headers, body) = send(
        &deployment.router,
        "POST",
        "/auth/password-reset/confirm",
        Some(&json!({ "token": reset, "password": password })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
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

                let user_key = format!("{USER_KEY_PREFIX}{user_id}");
                let indexed = connection
                    .zrange(&user_key, 0, -1)
                    .await
                    .expect("Redis must answer");
                assert!(
                    indexed.contains(&token_hash),
                    "the user's index must name the session: {indexed:?}",
                );

                // The index lives as long as the user's last session.
                let index_seconds_left = connection
                    .ttl(&user_key)
                    .await
                    .expect("Redis must answer")
                    .raw();
                assert!(
                    i64::try_from(index_seconds_left)
                        .is_ok_and(|seconds| seconds > lifetime_less_an_hour),
                    "the index must expire with the last session: {index_seconds_left}s left",
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

    /// Rewrites the generation the session behind `token` is stamped with.
    async fn stamp(&self, pool: &DbPool, token: &str, generation: i64) {
        let token_hash = hash(token);
        match self {
            Self::Postgres => {
                let mut connection = pool.get().await.expect("a connection");
                let updated =
                    diesel::update(sessions::table.filter(sessions::token_hash.eq(&token_hash)))
                        .set(sessions::generation.eq(generation))
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
                session["generation"] = json!(generation);
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

    /// Overwrites the session behind `token` with a value that is not a
    /// session, the way a foreign writer or a future format would. Only Redis
    /// holds a value that can stop parsing, so for Postgres this answers
    /// `false` and does nothing.
    async fn corrupt(&self, token: &str) -> bool {
        let Self::Redis { connection, .. } = self else {
            return false;
        };
        let mut connection = connection.clone();
        redis::cmd("SET")
            .arg(format!("{TOKEN_KEY_PREFIX}{}", hash(token)))
            .arg("this is not a session")
            .arg("KEEPTTL")
            .exec_async(&mut connection)
            .await
            .expect("the session must be overwritten");
        true
    }

    /// Drops the user's index, the key a Redis under memory pressure evicts
    /// first, because only signing in and the account's own pages touch it.
    ///
    /// A sign-out-everywhere then finds nothing to delete, and the session
    /// keys it should have deleted live on. The table cannot lose track of its
    /// rows that way, so for Postgres this does nothing.
    async fn evict_index(&self, user_id: Uuid) {
        if let Self::Redis { connection, .. } = self {
            let mut connection = connection.clone();
            connection
                .del(format!("{USER_KEY_PREFIX}{user_id}"))
                .await
                .expect("Redis must answer");
        }
    }

    /// Copies the session behind `token` exactly as the store holds it, to
    /// put back after it has ended.
    async fn remember(&self, pool: &DbPool, token: &str) -> Leftover {
        let token_hash = hash(token);
        match self {
            Self::Postgres => {
                let mut connection = pool.get().await.expect("a connection");
                let row = sessions::table
                    .filter(sessions::token_hash.eq(&token_hash))
                    .select((
                        sessions::id,
                        sessions::user_id,
                        sessions::created_at,
                        sessions::expires_at,
                        sessions::generation,
                    ))
                    .first(&mut connection)
                    .await
                    .expect("the session must be a row");
                Leftover::Row { token_hash, row }
            }
            Self::Redis { connection, .. } => {
                let mut connection = connection.clone();
                let stored = connection
                    .get(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
                    .await
                    .expect("Redis must answer")
                    .expect("the session must be a key");
                Leftover::Key { token_hash, stored }
            }
        }
    }

    /// Puts a remembered session back, index entry and all, the way a store
    /// restored from a snapshot taken before it ended would.
    async fn bring_back(&self, pool: &DbPool, leftover: Leftover) {
        match (self, leftover) {
            (Self::Postgres, Leftover::Row { token_hash, row }) => {
                let (id, user_id, created_at, expires_at, generation) = row;
                let mut connection = pool.get().await.expect("a connection");
                diesel::insert_into(sessions::table)
                    .values((
                        sessions::id.eq(id),
                        sessions::user_id.eq(user_id),
                        sessions::token_hash.eq(token_hash),
                        sessions::created_at.eq(created_at),
                        sessions::expires_at.eq(expires_at),
                        sessions::generation.eq(generation),
                    ))
                    .execute(&mut connection)
                    .await
                    .expect("the row must go back");
            }
            (Self::Redis { connection, .. }, Leftover::Key { token_hash, stored }) => {
                let session: Value = serde_json::from_str(&stored).expect("a session is JSON");
                let user_id = session["user_id"].as_str().expect("a user id");
                let ends_at: DateTime<Utc> =
                    serde_json::from_value(session["expires_at"].clone()).expect("an end");
                let mut connection = connection.clone();
                redis::pipe()
                    .cmd("SET")
                    .arg(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
                    .arg(&stored)
                    .arg("EXAT")
                    .arg(ends_at.timestamp())
                    .cmd("ZADD")
                    .arg(format!("{USER_KEY_PREFIX}{user_id}"))
                    .arg(ends_at.timestamp())
                    .arg(&token_hash)
                    .exec_async(&mut connection)
                    .await
                    .expect("the session must go back");
            }
            _ => unreachable!("a leftover goes back to the store it came from"),
        }
    }
}

/// A session copied out of its store, as [`Store::remember`] found it.
enum Leftover {
    Row {
        token_hash: String,
        row: (Uuid, Uuid, DateTime<Utc>, DateTime<Utc>, i64),
    },
    Key {
        token_hash: String,
        stored: String,
    },
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
    let first_id = listed_id(&listed, 0);
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

    // A stored session that no longer parses still signs its browser out:
    // the key is gone either way, so the sign-out answers as one that worked.
    let unreadable = sign_in(router, &email, PASSWORD).await;
    if store.corrupt(&unreadable).await {
        let (status, headers, _body) =
            send(router, "POST", "/auth/logout", None, Some(&unreadable)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            headers
                .get_all(axum::http::header::SET_COOKIE)
                .iter()
                .any(|cookie| cookie
                    .to_str()
                    .is_ok_and(|value| value.contains("Max-Age=0"))),
            "the cookie is cleared: {headers:?}",
        );
        assert_eq!(
            deployment.whoami(&unreadable).await,
            StatusCode::UNAUTHORIZED
        );
    } else {
        let (status, _headers, _body) =
            send(router, "POST", "/auth/logout", None, Some(&unreadable)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

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
    // The store loses track of the first session beforehand, the way an
    // evicted index does, so the cleanup cannot reach it; ending it has to be
    // decided in Postgres or it survives.
    let leftover = store.remember(&deployment.pool, &first).await;
    store.evict_index(user_id).await;
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

    // A store restored from a snapshot taken before the reset brings the
    // session's data back, and still nobody is signed in by it.
    store.bring_back(&deployment.pool, leftover).await;
    assert_eq!(
        deployment.whoami(&first).await,
        StatusCode::UNAUTHORIZED,
        "a session a sign-out-everywhere ended stays ended, whatever the store still holds",
    );

    // ---- An operator's temporary password signs out everywhere too.
    let rescued = sign_in(router, &email, RESET_PASSWORD).await;
    let listed = deployment.listed(&rescued).await;
    assert_eq!(
        listed.len(),
        1,
        "an ended session the store brought back is not listed: {listed:?}",
    );
    let (status, _headers, _body) = send(
        router,
        "DELETE",
        &format!("/auth/sessions/{first_id}"),
        None,
        Some(&rescued),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an ended session cannot be signed out again",
    );
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
    // The sweep removes the sessions the temporary password ended. The key
    // the snapshot brought back carries an older generation, so it is not
    // this sweep's to judge; it signs nobody in, as above, and expires on its
    // own.
    for token in [&rescued, &also_rescued] {
        assert_eq!(deployment.whoami(token).await, StatusCode::UNAUTHORIZED);
        store.assert_gone(&deployment.pool, user_id, token).await;
    }
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
        10,
        "registration and nine password sign-ins, each recorded once",
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
