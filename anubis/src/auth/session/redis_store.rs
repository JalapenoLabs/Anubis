//! Sessions kept in Redis, for an application that sets `SESSION_STORE=redis`.
//!
//! Two kinds of key, both under the framework's own `anubis:sessions:` prefix:
//!
//! | Key | Holds | Expires |
//! |---|---|---|
//! | `anubis:sessions:token:<sha256>` | the session, as JSON: its id, the user's id, when it was created and when it ends | when the session ends |
//! | `anubis:sessions:user:<user id>` | a sorted set of that user's token hashes, each scored by when it ends | when the user's last session ends |
//!
//! The token key answers the question every request asks, which user a cookie
//! belongs to, in one read. The user's index answers the questions only the
//! account asks: which sessions it has, and which to sign out. A key names the
//! token's SHA-256 and never the token, the same discipline the `sessions`
//! table keeps, so a Redis dump or a stolen snapshot holds nothing a browser
//! could present.
//!
//! Redis drops a token key on its own when the session ends, and every read
//! checks the end time again on the application's clock, which has the last
//! word in Postgres too. Index entries do not expire one by one, so an ended
//! session's entry is pruned whenever its user signs in and skipped by every
//! read, the same moments the Postgres store sweeps and filters its rows.
//!
//! Writes that touch both keys run as one `MULTI` block, so no reader sees a
//! session without its index entry. Signing everyone out removes exactly the
//! sessions it read from the index; a sign-in racing it survives, as it does
//! against the Postgres store's `DELETE`, and stays listed and revocable.
//!
//! `EXPIREAT ... NX` and `GT` need Redis 7.0 or later.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use redis::aio::ConnectionManager;
use redis::{AsyncTypedCommands, SetExpiry, SetOptions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Error, ListedSession, SESSION_TTL_DAYS};
use crate::auth::model::User;
use crate::auth::token;
use crate::schema::users;

/// Prefixes a token hash to name the key holding that session.
const TOKEN_KEY_PREFIX: &str = "anubis:sessions:token:";

/// Prefixes a user id to name the key indexing that user's sessions.
const USER_KEY_PREFIX: &str = "anubis:sessions:user:";

/// One session, as its token key holds it.
///
/// The token is absent on purpose: the key already names its hash, and the
/// value is what a reader of the account's session list may learn.
#[derive(Debug, Serialize, Deserialize)]
struct StoredSession {
    id: Uuid,
    user_id: Uuid,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// The Redis session store: one self-reconnecting connection, cheap to clone.
#[derive(Clone)]
pub(super) struct RedisSessions {
    connection: ConnectionManager,
}

impl RedisSessions {
    /// A store over the Redis at `url`, connecting on first use.
    ///
    /// # Panics
    /// Panics when `url` is not a Redis connection URL, which a URL from
    /// [`crate::config::RedisConfig`] never is: the configuration parsed it
    /// when it loaded.
    pub(super) fn new(url: &str) -> Self {
        let connection = crate::redis_connection::open(url)
            .expect("REDIS_URL was parsed when the configuration loaded");
        Self { connection }
    }

    /// Asks Redis to answer, connecting if this is the first command.
    pub(super) async fn verify(&self) -> Result<(), Error> {
        let mut redis_connection = self.connection.clone();
        redis_connection.ping().await?;
        Ok(())
    }

    /// Creates a session for `user_id` and returns the raw token for the cookie.
    ///
    /// Also prunes the user's ended sessions from their index, the moment the
    /// Postgres store sweeps its expired rows.
    pub(super) async fn create(&self, user_id: Uuid) -> Result<String, Error> {
        let token = token::generate();
        let token_hash = token::hash(&token);
        let now = Utc::now();
        let session = StoredSession {
            id: Uuid::new_v4(),
            user_id,
            created_at: now,
            expires_at: now + Duration::days(SESSION_TTL_DAYS),
        };
        let ends_at = session.expires_at.timestamp();
        let ends_at_unsigned =
            u64::try_from(ends_at).expect("a session created now ends after 1970");
        let stored = serde_json::to_string(&session)
            .expect("a session of ids and timestamps always serializes");
        let user_key = format!("{USER_KEY_PREFIX}{user_id}");

        let mut redis_connection = self.connection.clone();
        redis::pipe()
            .atomic()
            .set_options(
                format!("{TOKEN_KEY_PREFIX}{token_hash}"),
                stored,
                SetOptions::default().with_expiration(SetExpiry::EXAT(ends_at_unsigned)),
            )
            .ignore()
            .zadd(&user_key, &token_hash, ends_at)
            .ignore()
            // Inclusive of now: a session that ends at this second has ended,
            // as `expires_at <= now()` has in Postgres.
            .zrembyscore(&user_key, "-inf", now.timestamp())
            .ignore()
            // The index ends with the user's latest session, and only ever
            // moves later: `NX` gives a new index its end, and `GT` extends an
            // existing one. Taking the new session's end unconditionally would
            // be right only while every session lives the same length, and an
            // index that ended early would hide a live session from the next
            // sign-out-everywhere. `GT` alone never sets a first end, because
            // Redis counts a key without one as never ending.
            .cmd("EXPIREAT")
            .arg(&user_key)
            .arg(ends_at)
            .arg("NX")
            .ignore()
            .cmd("EXPIREAT")
            .arg(&user_key)
            .arg(ends_at)
            .arg("GT")
            .ignore()
            .exec_async(&mut redis_connection)
            .await?;

        Ok(token)
    }

    /// Resolves a raw session token to its user, if the session is live.
    ///
    /// The user is read from Postgres on every call, never kept in Redis, so a
    /// temporary password, a changed role, or a deleted account takes effect
    /// on the very next request. A session whose account is gone resolves to
    /// nobody.
    pub(super) async fn find_user(
        &self,
        connection: &mut AsyncPgConnection,
        token: &str,
    ) -> Result<Option<User>, Error> {
        let token_hash = token::hash(token);
        let mut redis_connection = self.connection.clone();
        let Some(stored) = redis_connection
            .get(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
            .await?
        else {
            return Ok(None);
        };

        let session: StoredSession = serde_json::from_str(&stored).map_err(Error::corrupt)?;
        if session.expires_at <= Utc::now() {
            return Ok(None);
        }

        let user = users::table
            .find(session.user_id)
            .select(User::as_select())
            .first(connection)
            .await
            .optional()?;
        Ok(user)
    }

    /// Deletes the session behind a raw token, signing that browser out.
    pub(super) async fn revoke(&self, token: &str) -> Result<(), Error> {
        let token_hash = token::hash(token);
        let mut redis_connection = self.connection.clone();
        let Some(removed) = redis_connection
            .get_del(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
            .await?
        else {
            tracing::debug!("a sign-out named a session that had already ended");
            return Ok(());
        };

        let session: StoredSession = serde_json::from_str(&removed).map_err(Error::corrupt)?;
        redis_connection
            .zrem(format!("{USER_KEY_PREFIX}{}", session.user_id), &token_hash)
            .await?;
        Ok(())
    }

    /// Deletes every session the user has, except the one behind `kept_token`.
    ///
    /// Removes exactly the sessions it read from the user's index, so a
    /// session created while it runs survives, listed and revocable, rather
    /// than losing its index entry and outliving every later sign-out.
    pub(super) async fn revoke_all(
        &self,
        user_id: Uuid,
        kept_token: Option<&str>,
    ) -> Result<(), Error> {
        let user_key = format!("{USER_KEY_PREFIX}{user_id}");
        let kept_hash = kept_token.map(token::hash);
        let mut redis_connection = self.connection.clone();
        let indexed = redis_connection.zrange(&user_key, 0, -1).await?;

        let mut revoked_hashes = Vec::with_capacity(indexed.len());
        let mut revoked_keys = Vec::with_capacity(indexed.len());
        for token_hash in indexed {
            if kept_hash.as_ref() == Some(&token_hash) {
                continue;
            }
            revoked_keys.push(format!("{TOKEN_KEY_PREFIX}{token_hash}"));
            revoked_hashes.push(token_hash);
        }
        if revoked_hashes.is_empty() {
            return Ok(());
        }

        redis::pipe()
            .atomic()
            .del(revoked_keys)
            .ignore()
            .zrem(&user_key, revoked_hashes)
            .ignore()
            .exec_async(&mut redis_connection)
            .await?;
        Ok(())
    }

    /// Lists a user's live sessions, oldest first, marking the one behind
    /// `current_token`.
    pub(super) async fn list(
        &self,
        user_id: Uuid,
        current_token: Option<&str>,
    ) -> Result<Vec<ListedSession>, Error> {
        let current_hash = current_token.map(token::hash);

        let mut listed: Vec<ListedSession> = self
            .live(user_id)
            .await?
            .into_iter()
            .map(|(token_hash, session)| ListedSession {
                id: session.id,
                created_at: session.created_at,
                expires_at: session.expires_at,
                current: current_hash.as_ref() == Some(&token_hash),
            })
            .collect();
        listed.sort_by_key(|session| session.created_at);
        Ok(listed)
    }

    /// Deletes the user's session with `session_id`, answering whether one went.
    ///
    /// Only the user's own index is searched, so another account's session id
    /// finds nothing, exactly as the Postgres store's scoped `DELETE` does.
    pub(super) async fn revoke_by_id(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<bool, Error> {
        let Some((token_hash, _session)) = self
            .live(user_id)
            .await?
            .into_iter()
            .find(|(_token_hash, session)| session.id == session_id)
        else {
            return Ok(false);
        };

        let mut redis_connection = self.connection.clone();
        let (deleted, _unindexed): (usize, usize) = redis::pipe()
            .atomic()
            .del(format!("{TOKEN_KEY_PREFIX}{token_hash}"))
            .zrem(format!("{USER_KEY_PREFIX}{user_id}"), &token_hash)
            .query_async(&mut redis_connection)
            .await?;
        // A concurrent sign-out may have taken it between the read and here,
        // which is the same answer as never having found it.
        Ok(deleted > 0)
    }

    /// The user's live sessions with their token hashes, in index order.
    ///
    /// An index entry whose token key is gone, or whose session has ended, is
    /// skipped rather than reported: it is a session that no longer exists.
    async fn live(&self, user_id: Uuid) -> Result<Vec<(String, StoredSession)>, Error> {
        let now = Utc::now();
        let mut redis_connection = self.connection.clone();
        let indexed = redis_connection
            .zrangebyscore(
                format!("{USER_KEY_PREFIX}{user_id}"),
                now.timestamp(),
                "+inf",
            )
            .await?;
        if indexed.is_empty() {
            return Ok(Vec::new());
        }

        let token_keys: Vec<String> = indexed
            .iter()
            .map(|token_hash| format!("{TOKEN_KEY_PREFIX}{token_hash}"))
            .collect();
        let stored = redis_connection.mget(token_keys).await?;

        let mut live = Vec::with_capacity(indexed.len());
        for (token_hash, stored) in indexed.into_iter().zip(stored) {
            let Some(stored) = stored else {
                continue;
            };
            let session: StoredSession = serde_json::from_str(&stored).map_err(Error::corrupt)?;
            if session.expires_at > now {
                live.push((token_hash, session));
            }
        }
        Ok(live)
    }
}

impl std::fmt::Debug for RedisSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection knows the URL, and the URL may carry a password.
        f.write_str("RedisSessions(..)")
    }
}
