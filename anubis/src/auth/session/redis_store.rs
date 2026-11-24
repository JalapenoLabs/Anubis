//! Sessions kept in Redis, for an application that sets `SESSION_STORE=redis`.
//!
//! Two kinds of key, both under the framework's own `anubis:sessions:` prefix:
//!
//! | Key | Holds | Expires |
//! |---|---|---|
//! | `anubis:sessions:token:<sha256>` | the session, as JSON: its id, the user's id, the account's session generation it was stamped with, when it was created and when it ends | when the session ends |
//! | `anubis:sessions:user:<user id>` | a sorted set of that user's token hashes, each scored by when it ends | when the user's last session ends |
//!
//! The token key answers the question every request asks, which user a cookie
//! belongs to, in one read. The user's index answers the questions only the
//! account asks: which sessions it has, and which to clean up after signing
//! out everywhere. A key names the token's SHA-256 and never the token, the
//! same discipline the `sessions` table keeps, so a Redis dump or a stolen
//! snapshot holds nothing a browser could present.
//!
//! Redis is not where a sign-out-everywhere is decided. That happens in
//! Postgres, by advancing the account's session generation, and every read
//! here compares a session's stamp with the generation it reads beside the
//! account. Deleting the keys afterwards is housekeeping: a Redis that lost
//! the index, evicted it, or came back from an older snapshot cannot bring an
//! ended session back. Signing one session out is the exception, a delete in
//! Redis alone, so a Redis that loses that write brings that one session back
//! until it ends or the account signs out everywhere.
//!
//! Redis drops a token key on its own when the session ends, and every read
//! checks the end time again on the application's clock, which has the last
//! word in Postgres too. Index entries do not expire one by one, so an ended
//! session's entry is pruned whenever its user signs in and skipped by every
//! read, the same moments the Postgres store sweeps and filters its rows.
//!
//! Creating a session writes both keys in one `MULTI` block, so no reader sees
//! a session without its index entry. Signing one session out deletes its key
//! and then its index entry; an entry left behind between the two names a key
//! that is gone, which every read skips.
//!
//! `EXPIREAT ... NX` and `GT` need Redis 7.0 or later, which the server's boot
//! checks, along with eviction and persistence, through
//! [`RedisSessions::verify`].

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use redis::aio::ConnectionManager;
use redis::{AsyncTypedCommands, SetExpiry, SetOptions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::inspection::{self, Eviction, Persistence};
use super::{Error, ListedSession, SESSION_TTL_DAYS, current_generation};
use crate::auth::model::User;
use crate::auth::token;
use crate::db::DbPool;
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
    /// The account's session generation this session was stamped with.
    generation: i64,
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

    /// Asks Redis whether it can hold sessions, connecting if this is the
    /// first command.
    ///
    /// Refuses a version older than 7.0 and a policy that may evict keys,
    /// because either one breaks the store while it runs. Warns about
    /// persistence short of an append-only file, and about a setting Redis
    /// would not show, because those cost sign-ins or one-at-a-time sign-outs
    /// on a restart rather than correctness while it runs. A server that
    /// reports no version it can be held to, as some Redis-compatible proxies
    /// do, passes with a warning: refusing it would refuse a store that may
    /// well work.
    pub(super) async fn verify(&self) -> Result<(), Error> {
        let mut redis_connection = self.connection.clone();
        let inspected = inspection::inspect(&mut redis_connection).await?;

        match (inspected.supports_version(), inspected.version.as_deref()) {
            (Some(false), Some(version)) => return Err(Error::unsupported(version)),
            (None, _) => tracing::warn!(
                "the Redis session store's server reports no version; the store needs Redis \
                 7.0 or later",
            ),
            _ => {}
        }

        match inspected.eviction {
            Eviction::Evicts { policy } => return Err(Error::evicting(&policy)),
            Eviction::Unreadable => tracing::warn!(
                "the Redis session store's eviction policy could not be read; a Redis that may \
                 evict keys drops sessions from the session list, so confirm it uses noeviction",
            ),
            Eviction::Never => {}
        }

        match inspected.persistence {
            Persistence::AppendOnlyFile => {}
            Persistence::Snapshots => tracing::warn!(
                "the Redis session store persists RDB snapshots only; a restart loses the \
                 sign-ins since the last one and brings back sessions signed out one at a time \
                 since then. Set appendonly yes",
            ),
            Persistence::Nothing => tracing::warn!(
                "the Redis session store persists nothing; a Redis restart signs everyone out. \
                 Set appendonly yes",
            ),
            Persistence::Unreadable => tracing::warn!(
                "the Redis session store's persistence could not be read; without persistence a \
                 Redis restart signs everyone out, so confirm it keeps an append-only file",
            ),
        }
        Ok(())
    }

    /// Creates a session for `user_id`, stamped with `generation`, and returns
    /// the raw token for the cookie.
    ///
    /// Also prunes the user's ended sessions from their index, the moment the
    /// Postgres store sweeps its expired rows.
    pub(super) async fn create(&self, user_id: Uuid, generation: i64) -> Result<String, Error> {
        let token = token::generate();
        let token_hash = token::hash(&token);
        let now = Utc::now();
        let session = StoredSession {
            id: Uuid::new_v4(),
            user_id,
            generation,
            created_at: now,
            expires_at: now + Duration::days(SESSION_TTL_DAYS),
        };
        let ends_at = session.expires_at.timestamp();
        let ends_at_unsigned =
            u64::try_from(ends_at).expect("a session created now ends after 1970");
        let stored = serde_json::to_string(&session)
            .expect("a session of ids, numbers, and timestamps always serializes");
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
            // index that ended early would hide a live session from the list.
            // `GT` alone never sets a first end, because Redis counts a key
            // without one as never ending.
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
    /// The account is read from Postgres on every call, never kept in Redis,
    /// together with its session generation, so a temporary password, a
    /// sign-out-everywhere, a changed role, or a deleted account takes effect
    /// on the very next request. The connection is checked out only once
    /// Redis has named a session, so waiting on Redis never holds one, and a
    /// token naming no session costs none.
    pub(super) async fn find_user(
        &self,
        pool: &DbPool,
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

        let mut connection = pool.get().await?;
        let account: Option<(User, i64)> = users::table
            .find(session.user_id)
            .select((User::as_select(), users::session_generation))
            .first(&mut connection)
            .await
            .optional()?;

        let user = account
            .filter(|(_user, generation)| *generation == session.generation)
            .map(|(user, _generation)| user);
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

        // The key is gone either way, so a value that does not parse still
        // signs this browser out; only its index entry cannot be named, and
        // every read skips an entry whose key is gone.
        let session: StoredSession = match serde_json::from_str(&removed) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    error.message = %error,
                    "a sign-out removed a session whose stored value does not parse, and left \
                     its index entry behind: {{error.message}}",
                );
                return Ok(());
            }
        };
        redis_connection
            .zrem(format!("{USER_KEY_PREFIX}{}", session.user_id), &token_hash)
            .await?;
        Ok(())
    }

    /// Re-stamps the session behind `keep_hash` with `generation`.
    ///
    /// Called inside a password change, after the account's generation moved
    /// and before it commits, so the browser making the change stays signed
    /// in. `SET ... XX` writes only a key that still exists, so a sign-out
    /// racing this is not undone, and `KEEPTTL` keeps the session's end.
    pub(super) async fn restamp(&self, keep_hash: &str, generation: i64) -> Result<(), Error> {
        let key = format!("{TOKEN_KEY_PREFIX}{keep_hash}");
        let mut redis_connection = self.connection.clone();
        let Some(stored) = redis_connection.get(&key).await? else {
            tracing::debug!("a password change kept a session that had already ended");
            return Ok(());
        };

        let mut session: StoredSession = serde_json::from_str(&stored).map_err(Error::corrupt)?;
        session.generation = generation;
        let restamped = serde_json::to_string(&session)
            .expect("a session of ids, numbers, and timestamps always serializes");
        redis::cmd("SET")
            .arg(&key)
            .arg(restamped)
            .arg("XX")
            .arg("KEEPTTL")
            .exec_async(&mut redis_connection)
            .await?;
        Ok(())
    }

    /// Deletes the keys of every session the user has that is not stamped
    /// with `generation`.
    ///
    /// Housekeeping after a sign-out-everywhere has committed: those sessions
    /// ended when the account moved to `generation`, so this only stops them
    /// taking up memory until they expire. A session stamped with
    /// `generation`, one created since the commit or the one a password change
    /// kept, stays, and so does its index entry. With no generation, the
    /// account is gone and everything goes.
    pub(super) async fn sweep(&self, user_id: Uuid, generation: Option<i64>) -> Result<(), Error> {
        let user_key = format!("{USER_KEY_PREFIX}{user_id}");
        let mut redis_connection = self.connection.clone();
        let indexed = redis_connection.zrange(&user_key, 0, -1).await?;
        if indexed.is_empty() {
            return Ok(());
        }
        let token_keys: Vec<String> = indexed
            .iter()
            .map(|token_hash| format!("{TOKEN_KEY_PREFIX}{token_hash}"))
            .collect();
        let stored = redis_connection.mget(token_keys.clone()).await?;

        let mut swept_hashes = Vec::with_capacity(indexed.len());
        let mut swept_keys = Vec::with_capacity(indexed.len());
        for ((token_hash, token_key), stored) in indexed.into_iter().zip(token_keys).zip(stored) {
            // A key that is gone or does not parse ends with the rest; only a
            // session carrying the new generation is still alive.
            let stamp = stored
                .and_then(|stored| serde_json::from_str::<StoredSession>(&stored).ok())
                .map(|session| session.generation);
            if generation.is_some() && stamp == generation {
                continue;
            }
            swept_keys.push(token_key);
            swept_hashes.push(token_hash);
        }
        if swept_hashes.is_empty() {
            return Ok(());
        }

        redis::pipe()
            .atomic()
            .del(swept_keys)
            .ignore()
            .zrem(&user_key, swept_hashes)
            .ignore()
            .exec_async(&mut redis_connection)
            .await?;
        Ok(())
    }

    /// Lists a user's live sessions, oldest first, marking the one behind
    /// `current_token`.
    pub(super) async fn list(
        &self,
        pool: &DbPool,
        user_id: Uuid,
        current_token: Option<&str>,
    ) -> Result<Vec<ListedSession>, Error> {
        let current_hash = current_token.map(token::hash);

        let mut listed: Vec<ListedSession> = self
            .live(pool, user_id)
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

    /// Deletes the user's live session with `session_id`, answering whether
    /// one went.
    ///
    /// Only the user's own sessions are searched, so another account's
    /// session id finds nothing, exactly as the Postgres store's scoped
    /// `DELETE` does.
    pub(super) async fn revoke_by_id(
        &self,
        pool: &DbPool,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<bool, Error> {
        let Some((token_hash, _session)) = self
            .live(pool, user_id)
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
    /// An index entry whose token key is gone, whose session has ended, or
    /// whose stamp is not the account's current generation is skipped rather
    /// than reported: it is a session that no longer exists. Redis is read
    /// first and the generation after, so no connection waits on Redis.
    async fn live(
        &self,
        pool: &DbPool,
        user_id: Uuid,
    ) -> Result<Vec<(String, StoredSession)>, Error> {
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

        let mut connection = pool.get().await?;
        let Some(generation) = current_generation(&mut connection, user_id).await? else {
            return Ok(Vec::new());
        };
        drop(connection);

        let mut live = Vec::with_capacity(indexed.len());
        for (token_hash, stored) in indexed.into_iter().zip(stored) {
            let Some(stored) = stored else {
                continue;
            };
            let session: StoredSession = serde_json::from_str(&stored).map_err(Error::corrupt)?;
            if session.expires_at > now && session.generation == generation {
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
