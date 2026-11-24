//! Opaque-token browser sessions, kept in Postgres or in Redis.
//!
//! A session is a random 256-bit token handed to the browser in an `HttpOnly`
//! cookie. The store keeps only the token's SHA-256, so a leaked database or a
//! Redis dump never yields usable session tokens, and the token itself is
//! unforgeable without guessing 256 bits. Expiry is enforced server-side on
//! every lookup, and ended sessions are pruned when their user signs in again.
//!
//! [`Sessions`] is the one way in, whichever store `SESSION_STORE` chose:
//! Postgres by default, or Redis. Behaviour is identical in both, down to the
//! order the session list comes back in, and every caller goes through the
//! same methods, so nothing above this module knows which store answered.
//!
//! # Signing out everywhere is decided in Postgres
//!
//! Every account carries a session generation (`users.session_generation`),
//! and every session is stamped with the generation it was created under. A
//! session signs nobody in unless its stamp equals the account's current
//! generation, which every lookup reads beside the account itself. Signing out
//! everywhere advances the generation, in the same transaction as the
//! credential change that called for it, so the sessions end the moment that
//! transaction commits and not a moment before, in either store. Deleting
//! them afterwards is housekeeping, done after the commit by
//! [`Sessions::sweep`]: a store that loses those deletes, evicts the keys that
//! would have found them, comes back from an older snapshot, or was the other
//! store when `SESSION_STORE` changed, cannot bring an ended session back.
//!
//! The stamp is compared for equality, not order. A password change keeps the
//! browser that made it by re-stamping that session with the new generation
//! before the commit; in Redis that write cannot roll back, so if the commit
//! then fails the kept session carries a stamp one ahead of the account, and
//! equality signs it out rather than letting it outlive the next
//! sign-out-everywhere. The price is a few milliseconds, between that
//! re-stamp and the commit, in which another request from the same browser
//! is answered `401`.
//!
//! Signing one session out, from the list or by signing out, deletes that
//! session from its store and moves no generation, so it is only as durable as
//! the store's delete. A Redis that loses the write brings that one session
//! back until it ends or the account signs out everywhere.
//!
//! # How a request finds its store
//!
//! [`crate::server::harden`] puts the configured [`Sessions`] on every request
//! as an extension, once, for every router the application composed. The
//! extractors read it from there with [`Sessions::for_request`], and a request
//! that never passed through the hardened stack, such as one a test sends
//! straight to a bare router, uses the Postgres default. That is what keeps
//! "unset, nothing changes" true for every application and every test that
//! never names a store. The authentication routes, which are the ones that
//! write sessions, hold that default to the configuration: there a request
//! carrying a store the configuration did not choose, or none while it chose
//! Redis, is refused with a logged `500` rather than answered by the wrong
//! store.
//!
//! # Transactions
//!
//! The Postgres store writes through the caller's connection, so a session
//! commits or rolls back with the transaction around it. Redis cannot join
//! that transaction, so callers order their work to keep failures safe:
//!
//! - **Creating** a session is the last step inside the transaction that
//!   records the sign-in, and anything spent to sign in, a reset link or a
//!   recovery code, is spent in that same transaction. A Redis failure rolls
//!   all of it back, and the cookie is only issued after the commit, so no
//!   browser ever holds a session the audit log does not know about. A commit
//!   that fails after the write leaves a session no browser holds, which ends
//!   with its expiry.
//! - **Signing out everywhere** advances the generation inside the
//!   transaction, as its last statement, and sweeps the store after the
//!   commit. Nothing there waits on Redis while holding the account's row.

mod inspection;
mod postgres_store;
mod redis_store;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::PoolError;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::model::User;
use crate::auth::routes::AuthState;
use crate::auth::token;
use crate::config::{AppConfig, SessionStore};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::schema::users;
use redis_store::RedisSessions;

pub use inspection::{Eviction, Persistence, RedisInspection, inspect_redis};

/// Name of the session cookie set at registration and login.
pub const SESSION_COOKIE: &str = "anubis_session";

/// How long a session lives. Fixed expiry; sliding renewal can come later.
pub const SESSION_TTL_DAYS: i64 = 30;

/// How an account proved who it was before a session was issued.
///
/// Recorded on the [`crate::audit::SESSION_CREATED`] event every sign-in
/// writes, so a reader of the log can tell a password from a passkey. The
/// spellings are the event's contract with whoever reads it, which is why they
/// are constants here rather than strings at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInMethod {
    /// Registering signs the new account in.
    Registration,
    /// An email address and a password, with no second factor confirmed.
    Password,
    /// A TOTP or recovery code, completing a password or emailed-code sign-in
    /// on an account with a confirmed second factor.
    SecondFactor,
    /// A one-time code mailed to the address.
    EmailCode,
    /// A passkey, which is possession and verification in one step.
    Passkey,
    /// An OpenID Connect provider vouching for the address.
    Oauth,
    /// Accepting an operator's invitation, which creates the account.
    Invitation,
}

impl SignInMethod {
    /// Every method, in the order a sign-in page offers them.
    pub const ALL: [Self; 7] = [
        Self::Registration,
        Self::Password,
        Self::SecondFactor,
        Self::EmailCode,
        Self::Passkey,
        Self::Oauth,
        Self::Invitation,
    ];

    /// The spelling the audit event records.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::Password => "password",
            Self::SecondFactor => "second_factor",
            Self::EmailCode => "email_code",
            Self::Passkey => "passkey",
            Self::Oauth => "oauth",
            Self::Invitation => "invitation",
        }
    }
}

/// One live session, as `GET /auth/sessions` answers with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ListedSession {
    pub(crate) id: Uuid,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
    /// True for the session making this request.
    pub(crate) current: bool,
}

/// Every session of one account, ended in Postgres, left to sweep from the
/// store once the transaction that ended them commits.
///
/// Returned by [`Sessions::revoke_all`] and [`Sessions::revoke_all_except`].
/// The sessions are already dead the moment that transaction commits; handing
/// this to [`Sessions::sweep`] afterwards only removes what is left of them.
#[derive(Debug)]
#[must_use = "sweep the ended sessions once the transaction has committed"]
pub(crate) struct Revocation {
    user_id: Uuid,
    /// The hash of the session a password change kept, which the sweep spares.
    kept_hash: Option<String>,
}

/// Where browser sessions are kept, and every way to read or change them.
///
/// Cheap to clone: the Postgres store holds nothing, and the Redis store holds
/// one shared, self-reconnecting connection. Build it with
/// [`Sessions::from_config`]; a request finds it with
/// [`Sessions::for_request`], and an authentication route by taking it as an
/// extractor.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sessions {
    store: Store,
}

/// The store behind a [`Sessions`].
#[derive(Debug, Clone, Default)]
enum Store {
    /// The `sessions` table, through the caller's connection.
    #[default]
    Postgres,
    /// The Redis `REDIS_URL` names.
    Redis(RedisSessions),
}

impl Sessions {
    /// The store the configuration chose.
    ///
    /// Performs no I/O: a Redis store connects on its first command, so a
    /// caller that must know Redis can serve calls [`Sessions::verify`].
    ///
    /// # Panics
    /// Panics when the configuration chose Redis but carries no Redis URL,
    /// which [`AppConfig`] refuses to load, so only a hand-built configuration
    /// breaking that rule reaches it. Must be called inside a tokio runtime
    /// when the store is Redis.
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        match config.session_store {
            SessionStore::Postgres => Self::default(),
            SessionStore::Redis => {
                let redis = config
                    .redis
                    .as_ref()
                    .expect("the configuration refuses SESSION_STORE=redis without REDIS_URL");
                Self {
                    store: Store::Redis(RedisSessions::new(redis.url())),
                }
            }
        }
    }

    /// The store a request carries, or the Postgres default for one that has
    /// not been through [`crate::server::harden`]; see the module docs.
    pub(crate) fn for_request(parts: &Parts) -> Self {
        if let Some(sessions) = parts.extensions.get::<Self>() {
            return sessions.clone();
        }
        tracing::debug!(
            "this request carries no session store, because it did not pass through \
             anubis::server::harden; reading sessions from Postgres",
        );
        Self::default()
    }

    /// Which store this is, in the configuration's terms.
    fn kind(&self) -> SessionStore {
        match self.store {
            Store::Postgres => SessionStore::Postgres,
            Store::Redis(_) => SessionStore::Redis,
        }
    }

    /// Proves the store can serve, which is what the server's boot asks first.
    ///
    /// The Postgres store needs nothing: the pool was proven when it was
    /// built. The Redis store asks Redis whether it can hold sessions; see
    /// [`inspect_redis`].
    ///
    /// # Errors
    /// Returns an [`Error`] when Redis cannot be reached, does not answer,
    /// reports a version older than 7.0, or may evict keys.
    pub(crate) async fn verify(&self) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => Ok(()),
            Store::Redis(redis) => redis.verify().await,
        }
    }

    /// Creates a session for `user_id` and returns the raw token for the cookie.
    ///
    /// Stamps it with the account's current session generation, read on
    /// `connection`. Call it as the last step of the transaction that records
    /// the sign-in; see the module docs.
    ///
    /// # Errors
    /// Returns an [`Error`] when the account cannot be read or the store
    /// cannot be written.
    pub(crate) async fn create(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
    ) -> Result<String, Error> {
        let generation: i64 = users::table
            .find(user_id)
            .select(users::session_generation)
            .first(connection)
            .await?;

        match &self.store {
            Store::Postgres => Ok(postgres_store::create(connection, user_id, generation).await?),
            Store::Redis(redis) => redis.create(user_id, generation).await,
        }
    }

    /// Resolves a raw session token to its user, if the session is live.
    ///
    /// Takes the pool rather than a connection, because this runs on every
    /// authenticated request: the Redis store reads the session before it
    /// checks a connection out, so a Redis that has stalled never holds
    /// Postgres connections everything else needs, and a token that names no
    /// session costs no connection at all.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store or the pool cannot answer. A token
    /// that names no live session is `Ok(None)`, never an error.
    pub(crate) async fn find_user(
        &self,
        pool: &DbPool,
        token: &str,
    ) -> Result<Option<User>, Error> {
        match &self.store {
            Store::Postgres => {
                let mut connection = pool.get().await?;
                Ok(postgres_store::find_user(&mut connection, token).await?)
            }
            Store::Redis(redis) => redis.find_user(pool, token).await,
        }
    }

    /// Ends the session behind a raw token, signing that browser out.
    ///
    /// A token that names no session is already signed out, and succeeds.
    /// Takes the pool so the Redis store never holds a connection it does not
    /// use.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be written.
    pub(crate) async fn revoke(&self, pool: &DbPool, token: &str) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => {
                let mut connection = pool.get().await?;
                Ok(postgres_store::revoke(&mut connection, token).await?)
            }
            Store::Redis(redis) => redis.revoke(token).await,
        }
    }

    /// Ends every session the user has, signing out all of their browsers.
    ///
    /// Advances the account's session generation on `connection`, which is
    /// what ends them; call it as the last statement of the transaction that
    /// changes the credentials, and hand the [`Revocation`] to
    /// [`Sessions::sweep`] once that transaction commits. An account that no
    /// longer exists, one being deleted, has no generation to advance and
    /// nothing to fail.
    ///
    /// # Errors
    /// Returns an [`Error`] when the database cannot be written.
    pub(crate) async fn revoke_all(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
    ) -> Result<Revocation, Error> {
        advance_generation(connection, user_id).await?;
        if let Store::Postgres = self.store {
            postgres_store::delete_all(connection, user_id).await?;
        }
        Ok(Revocation {
            user_id,
            kept_hash: None,
        })
    }

    /// Ends every session the user has except the one behind `keep_token`.
    ///
    /// Called inside a signed-in password change: the generation advances and
    /// the kept session is re-stamped with it, so other browsers sign out and
    /// the browser that made the change stays signed in. Hand the
    /// [`Revocation`] to [`Sessions::sweep`] once the transaction commits.
    ///
    /// # Errors
    /// Returns an [`Error`] when the database cannot be written, or the Redis
    /// store cannot re-stamp the kept session, which keeps the browser from
    /// being signed out by its own password change.
    pub(crate) async fn revoke_all_except(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        keep_token: &str,
    ) -> Result<Revocation, Error> {
        let keep_hash = token::hash(keep_token);
        let Some(generation) = advance_generation(connection, user_id).await? else {
            tracing::debug!(
                user.id = %user_id,
                "a password change for {{user.id}} found no account to advance",
            );
            return Ok(Revocation {
                user_id,
                kept_hash: Some(keep_hash),
            });
        };

        match &self.store {
            Store::Postgres => {
                postgres_store::keep_only(connection, user_id, &keep_hash, generation).await?;
            }
            Store::Redis(redis) => redis.restamp(&keep_hash, generation).await?,
        }
        Ok(Revocation {
            user_id,
            kept_hash: Some(keep_hash),
        })
    }

    /// Removes what is left of the sessions a committed revocation ended.
    ///
    /// The Postgres store deleted its rows inside the transaction, so there is
    /// nothing left to do. The Redis store deletes the keys now, after the
    /// commit, so a Redis that is slow or away never holds the account's row
    /// or a connection; a failure only means the ended sessions take up memory
    /// until they expire, so it is logged and not returned.
    pub(crate) async fn sweep(&self, revocation: Revocation) {
        let Store::Redis(redis) = &self.store else {
            return;
        };
        if let Err(error) = redis
            .sweep(revocation.user_id, revocation.kept_hash.as_deref())
            .await
        {
            tracing::warn!(
                error.message = %error,
                user.id = %revocation.user_id,
                "the ended sessions of {{user.id}} could not be swept from Redis; they stay \
                 ended and expire on their own: {{error.message}}",
            );
        }
    }

    /// Lists the user's live sessions, oldest first, marking the one behind
    /// `current_token`.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store or the pool cannot answer.
    pub(crate) async fn list(
        &self,
        pool: &DbPool,
        user_id: Uuid,
        current_token: Option<&str>,
    ) -> Result<Vec<ListedSession>, Error> {
        match &self.store {
            Store::Postgres => {
                let mut connection = pool.get().await?;
                Ok(postgres_store::list(&mut connection, user_id, current_token).await?)
            }
            Store::Redis(redis) => redis.list(pool, user_id, current_token).await,
        }
    }

    /// Ends the user's session with `session_id`, answering whether one ended.
    ///
    /// Another account's session id ends nothing and answers `false`, so the
    /// route can answer it exactly as it answers an id that never existed.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store or the pool cannot answer.
    pub(crate) async fn revoke_by_id(
        &self,
        pool: &DbPool,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<bool, Error> {
        match &self.store {
            Store::Postgres => {
                let mut connection = pool.get().await?;
                Ok(postgres_store::revoke_by_id(&mut connection, user_id, session_id).await?)
            }
            Store::Redis(redis) => redis.revoke_by_id(pool, user_id, session_id).await,
        }
    }
}

impl FromRequestParts<AuthState> for Sessions {
    type Rejection = ApiError;

    /// The store the request carries, held to the store the configuration
    /// chose; see the module docs.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AuthState,
    ) -> Result<Self, Self::Rejection> {
        let configured = state.session_store;
        match parts.extensions.get::<Self>() {
            Some(sessions) if sessions.kind() == configured => Ok(sessions.clone()),
            None if configured == SessionStore::Postgres => Ok(Self::default()),
            Some(sessions) => {
                tracing::error!(
                    session.store.configured = %configured,
                    session.store.carried = %sessions.kind(),
                    "SESSION_STORE is {{session.store.configured}}, and this request carries the \
                     {{session.store.carried}} store; build every hardened router from one \
                     configuration",
                );
                Err(ApiError::internal())
            }
            None => {
                tracing::error!(
                    session.store.configured = %configured,
                    "SESSION_STORE is {{session.store.configured}}, and this request carries no \
                     session store; serve the authentication routes through \
                     anubis::server::harden, which puts the configured store on every request",
                );
                Err(ApiError::internal())
            }
        }
    }
}

/// The account's current session generation, or `None` when it is gone.
async fn current_generation(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<Option<i64>, diesel::result::Error> {
    users::table
        .find(user_id)
        .select(users::session_generation)
        .first(connection)
        .await
        .optional()
}

/// Advances the account's session generation, ending every session stamped
/// with an older one, and answers the new generation, or `None` when the
/// account is gone.
async fn advance_generation(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<Option<i64>, diesel::result::Error> {
    diesel::update(users::table.find(user_id))
        .set(users::session_generation.eq(users::session_generation + 1))
        .returning(users::session_generation)
        .get_result(connection)
        .await
        .optional()
}

/// A session could not be read or written.
///
/// Every cause is the server's, never the caller's: a request that meets one
/// answers `500` exactly as a failed query does, and is never mistaken for a
/// signed-out one. It converts from a Diesel error, so it can be the error
/// type of a transaction that also writes sessions.
#[derive(Debug)]
pub(crate) struct Error {
    kind: ErrorKind,
    backtrace: Backtrace,
}

#[derive(Debug)]
enum ErrorKind {
    Database(diesel::result::Error),
    /// No Postgres connection could be checked out to read the account.
    Pool(PoolError),
    Redis(redis::RedisError),
    /// A stored session that does not parse, which only something other than
    /// this store writing under its keys could produce.
    Corrupt(serde_json::Error),
    /// A Redis older than the commands the store sends, with the version it
    /// reported.
    Unsupported(String),
    /// A Redis that may evict keys, with the policy it reported.
    Evicting(String),
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::capture(),
        }
    }

    fn corrupt(source: serde_json::Error) -> Self {
        Self::new(ErrorKind::Corrupt(source))
    }

    fn unsupported(version: &str) -> Self {
        Self::new(ErrorKind::Unsupported(version.to_owned()))
    }

    fn evicting(policy: &str) -> Self {
        Self::new(ErrorKind::Evicting(policy.to_owned()))
    }
}

impl From<diesel::result::Error> for Error {
    fn from(source: diesel::result::Error) -> Self {
        Self::new(ErrorKind::Database(source))
    }
}

impl From<PoolError> for Error {
    fn from(source: PoolError) -> Self {
        Self::new(ErrorKind::Pool(source))
    }
}

impl From<redis::RedisError> for Error {
    fn from(source: redis::RedisError) -> Self {
        Self::new(ErrorKind::Redis(source))
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Database(source) => write!(f, "a session query failed: {source}")?,
            ErrorKind::Pool(source) => {
                write!(f, "no connection to read the session's account: {source}")?;
            }
            // Redis errors render the address but never the password.
            ErrorKind::Redis(source) => write!(f, "the Redis session store failed: {source}")?,
            ErrorKind::Corrupt(source) => {
                write!(f, "a session stored in Redis does not parse: {source}")?;
            }
            ErrorKind::Unsupported(version) => write!(
                f,
                "the Redis session store needs Redis 7.0 or later, and this one reports {version}",
            )?,
            ErrorKind::Evicting(policy) => write!(
                f,
                "the Redis session store needs a Redis that never evicts keys, and this one has \
                 a memory limit with maxmemory-policy {policy}; set noeviction",
            )?,
        }
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Database(source) => Some(source),
            ErrorKind::Pool(source) => Some(source),
            ErrorKind::Redis(source) => Some(source),
            ErrorKind::Corrupt(source) => Some(source),
            ErrorKind::Unsupported(_) | ErrorKind::Evicting(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Sessions, Store};
    use crate::config::AppConfig;

    fn config(pairs: &'static [(&'static str, &'static str)]) -> AppConfig {
        AppConfig::from_lookup(|name| {
            pairs
                .iter()
                .find(|(variable, _value)| *variable == name)
                .map(|(_variable, value)| (*value).to_owned())
        })
        .expect("the test configuration parses")
    }

    #[test]
    fn the_default_configuration_keeps_sessions_in_postgres() {
        let sessions = Sessions::from_config(&config(&[]));
        assert!(matches!(sessions.store, Store::Postgres));
    }

    #[test]
    fn a_redis_url_alone_keeps_sessions_in_postgres() {
        let sessions = Sessions::from_config(&config(&[("REDIS_URL", "redis://localhost:6379")]));
        assert!(matches!(sessions.store, Store::Postgres));
    }

    // A tokio test because the Redis store's connection spawns its supervisor;
    // nothing connects, since the connection is lazy.
    #[tokio::test]
    async fn the_redis_store_is_chosen_by_name_and_never_prints_its_url() {
        let sessions = Sessions::from_config(&config(&[
            ("SESSION_STORE", "redis"),
            ("REDIS_URL", "redis://default:hunter2@localhost:6379"),
        ]));
        assert!(matches!(sessions.store, Store::Redis(_)));

        let rendered = format!("{sessions:?}");
        assert!(rendered.contains("Redis"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }
}
