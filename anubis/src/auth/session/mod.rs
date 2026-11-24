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
//! # How a request finds its store
//!
//! [`crate::server::harden`] puts the configured [`Sessions`] on every request
//! as an extension, once, for every router the application composed. The
//! extractors and handlers read it from there with [`Sessions::for_request`],
//! and a request that never passed through the hardened stack, such as one a
//! test sends straight to a bare router, uses the Postgres default. That is
//! what keeps "unset, nothing changes" true for every application and every
//! test that never names a store.
//!
//! # Transactions
//!
//! Every method takes the caller's Postgres connection. The Postgres store
//! writes through it, so a session commits or rolls back with the transaction
//! around it. Redis cannot join that transaction, so callers order their work
//! to keep the failures safe:
//!
//! - **Creating** a session is the last step inside the transaction that
//!   records the sign-in. A Redis failure then rolls the sign-in back, and the
//!   cookie is only issued after the commit, so no browser ever holds a session
//!   the audit log does not know about. A commit that fails after the write
//!   leaves a session no browser holds, which ends with its expiry.
//! - **Revoking** for a password reset, a temporary password, a password
//!   change, or a deleted account happens inside the transaction, before the
//!   commit. A Redis failure aborts the change, and a commit that fails after
//!   the revocation leaves the person signed out with nothing else changed.
//!   Either way a stale session never outlives the credentials it was made with.

mod postgres_store;
mod redis_store;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::convert::Infallible;
use std::fmt::{self, Display, Formatter};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::deadpool::PoolError;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::model::User;
use crate::config::{AppConfig, SessionStore};
use crate::db::DbPool;
use redis_store::RedisSessions;

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

/// Where browser sessions are kept, and every way to read or change them.
///
/// Cheap to clone: the Postgres store holds nothing, and the Redis store holds
/// one shared, self-reconnecting connection. Build it with
/// [`Sessions::from_config`]; a request finds it with
/// [`Sessions::for_request`] or by taking it as an extractor.
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
    /// caller that must know Redis answers before serving calls
    /// [`Sessions::verify`].
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
        parts.extensions.get::<Self>().cloned().unwrap_or_default()
    }

    /// Proves the store can serve, which is what the server's boot asks first.
    ///
    /// The Postgres store needs nothing: the pool was proven when it was
    /// built. The Redis store asks Redis for its version, connecting if it had
    /// not yet, and refuses one older than the commands it sends.
    ///
    /// # Errors
    /// Returns an [`Error`] when Redis cannot be reached, does not answer, or
    /// reports a version older than 7.0.
    pub(crate) async fn verify(&self) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => Ok(()),
            Store::Redis(redis) => redis.verify().await,
        }
    }

    /// Creates a session for `user_id` and returns the raw token for the cookie.
    ///
    /// Call it as the last step of the transaction that records the sign-in;
    /// see the module docs.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be written.
    pub(crate) async fn create(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
    ) -> Result<String, Error> {
        match &self.store {
            Store::Postgres => Ok(postgres_store::create(connection, user_id).await?),
            Store::Redis(redis) => redis.create(user_id).await,
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
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be written.
    pub(crate) async fn revoke(
        &self,
        connection: &mut AsyncPgConnection,
        token: &str,
    ) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => Ok(postgres_store::revoke(connection, token).await?),
            Store::Redis(redis) => redis.revoke(token).await,
        }
    }

    /// Ends every session the user has, signing out all of their browsers.
    ///
    /// Called inside the transaction of a password reset, a temporary
    /// password, or an account deletion, before it commits; see the module
    /// docs.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be written.
    pub(crate) async fn revoke_all(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
    ) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => Ok(postgres_store::revoke_all(connection, user_id).await?),
            Store::Redis(redis) => redis.revoke_all(user_id, None).await,
        }
    }

    /// Ends every session the user has except the one behind `keep_token`.
    ///
    /// Called inside a signed-in password change: other browsers sign out, the
    /// browser that made the change stays signed in.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be written.
    pub(crate) async fn revoke_all_except(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        keep_token: &str,
    ) -> Result<(), Error> {
        match &self.store {
            Store::Postgres => {
                Ok(postgres_store::revoke_all_except(connection, user_id, keep_token).await?)
            }
            Store::Redis(redis) => redis.revoke_all(user_id, Some(keep_token)).await,
        }
    }

    /// Lists the user's live sessions, oldest first, marking the one behind
    /// `current_token`.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be read.
    pub(crate) async fn list(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        current_token: Option<&str>,
    ) -> Result<Vec<ListedSession>, Error> {
        match &self.store {
            Store::Postgres => Ok(postgres_store::list(connection, user_id, current_token).await?),
            Store::Redis(redis) => redis.list(user_id, current_token).await,
        }
    }

    /// Ends the user's session with `session_id`, answering whether one ended.
    ///
    /// Another account's session id ends nothing and answers `false`, so the
    /// route can answer it exactly as it answers an id that never existed.
    ///
    /// # Errors
    /// Returns an [`Error`] when the store cannot be read or written.
    pub(crate) async fn revoke_by_id(
        &self,
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<bool, Error> {
        match &self.store {
            Store::Postgres => {
                Ok(postgres_store::revoke_by_id(connection, user_id, session_id).await?)
            }
            Store::Redis(redis) => redis.revoke_by_id(user_id, session_id).await,
        }
    }
}

impl<S> FromRequestParts<S> for Sessions
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::for_request(parts))
    }
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
            ErrorKind::Unsupported(_version) => None,
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
