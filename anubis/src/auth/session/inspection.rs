//! What a Redis says about whether it can hold sessions.
//!
//! The server's boot and `anubis doctor` both ask, through [`inspect`] and
//! [`inspect_redis`], so the two can never disagree about what a session store
//! needs. Three things matter:
//!
//! - **The version.** The store's commands need Redis 7.0 or later.
//! - **Eviction.** A Redis with a memory limit and any policy but
//!   `noeviction` may drop session keys, and the index key goes first, because
//!   only signing in and the account's own pages touch it. A session whose
//!   index entry was dropped still signs in but is missing from the session
//!   list. Signing out everywhere still ends it, because that is decided in
//!   Postgres, but nothing shows it is there.
//! - **Persistence.** A Redis that persists nothing signs everyone out when it
//!   restarts. One that persists only RDB snapshots loses the sign-ins since
//!   the last one, and brings back sessions signed out one at a time since
//!   then. An append-only file loses about a second at most.
//!
//! Each reading is a pure function of what Redis reports, so every verdict is
//! unit-tested without a Redis. Eviction is read from `INFO memory`, which
//! reports `maxmemory` and `maxmemory_policy` and which managed Redis answers
//! even where it refuses `CONFIG`. Persistence is read with `CONFIG GET`, and
//! a Redis that refuses it falls back to `INFO persistence`, which can confirm
//! an append-only file but not the snapshot schedule.

use std::collections::HashMap;

use redis::aio::ConnectionLike;
use redis::{Client, RedisError};

/// The oldest Redis major version the session store runs on.
///
/// `EXPIREAT` gained `NX` and `GT` in 7.0, and the index's expiry relies on
/// both; an older Redis would refuse every sign-in.
const MINIMUM_MAJOR_VERSION: u32 = 7;

/// Everything a session store asks of its Redis, as one Redis answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisInspection {
    /// The version `INFO server` reported, when it reported one.
    pub version: Option<String>,
    /// Whether Redis may evict keys to stay under a memory limit.
    pub eviction: Eviction,
    /// What Redis does to keep its data across a restart.
    pub persistence: Persistence,
}

impl RedisInspection {
    /// Whether the reported version runs the session store: `None` when Redis
    /// reported no version whose major parses, as some Redis-compatible
    /// proxies do not.
    #[must_use]
    pub fn supports_version(&self) -> Option<bool> {
        let major: u32 = self.version.as_deref()?.split('.').next()?.parse().ok()?;
        Some(major >= MINIMUM_MAJOR_VERSION)
    }
}

/// Whether a Redis may evict keys to stay under a memory limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eviction {
    /// No memory limit, or `noeviction`: a full Redis refuses writes instead.
    Never,
    /// A memory limit, and a policy that drops keys to stay under it.
    Evicts {
        /// The `maxmemory-policy` Redis reported.
        policy: String,
    },
    /// `INFO memory` did not report the limit and the policy.
    Unreadable,
}

/// What a Redis does to keep its data across a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// It appends every write to a file it replays on start.
    AppendOnlyFile,
    /// It snapshots on a schedule, losing what changed since the last one.
    Snapshots,
    /// It keeps nothing: a restart starts empty.
    Nothing,
    /// It would not say.
    Unreadable,
}

/// Asks the Redis at `url` everything a session store needs from it.
///
/// For `anubis doctor`, which has a URL rather than the server's connection.
/// Hidden from the documentation because no application calls it.
///
/// # Errors
/// Returns the Redis error when `url` does not open a client, or Redis cannot
/// be reached or does not answer.
#[doc(hidden)]
pub async fn inspect_redis(url: &str) -> Result<RedisInspection, RedisError> {
    let client = Client::open(url)?;
    let mut connection = client.get_multiplexed_async_connection().await?;
    inspect(&mut connection).await
}

/// Asks the Redis behind `connection` everything a session store needs.
///
/// # Errors
/// Returns the Redis error when Redis cannot be reached or does not answer
/// `INFO`. A refused `CONFIG GET` is not an error: persistence is then read
/// from `INFO persistence` instead.
pub(crate) async fn inspect(
    connection: &mut (impl ConnectionLike + Send),
) -> Result<RedisInspection, RedisError> {
    let server: String = redis::cmd("INFO")
        .arg("server")
        .query_async(connection)
        .await?;
    let version = reported_version(&server).map(str::to_owned);

    let memory: String = redis::cmd("INFO")
        .arg("memory")
        .query_async(connection)
        .await?;
    let eviction = eviction_from_info(&memory);

    let settings = (
        config_value(connection, "appendonly").await,
        config_value(connection, "save").await,
    );
    let persistence = if let (Ok(appendonly), Ok(save)) = settings {
        persistence(&appendonly, &save)
    } else {
        let info: String = redis::cmd("INFO")
            .arg("persistence")
            .query_async(connection)
            .await?;
        persistence_from_info(&info)
    };

    Ok(RedisInspection {
        version,
        eviction,
        persistence,
    })
}

/// One setting's value from `CONFIG GET`, empty when Redis has no such setting.
async fn config_value(
    connection: &mut (impl ConnectionLike + Send),
    name: &str,
) -> Result<String, RedisError> {
    let mut settings: HashMap<String, String> = redis::cmd("CONFIG")
        .arg("GET")
        .arg(name)
        .query_async(connection)
        .await?;
    Ok(settings.remove(name).unwrap_or_default())
}

/// The value of the `name:value` line `name` in an `INFO` section.
fn info_field<'info>(info: &'info str, name: &str) -> Option<&'info str> {
    info.lines().find_map(|line| {
        line.trim()
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
    })
}

/// The version `INFO server` reports, when it reports one.
fn reported_version(info: &str) -> Option<&str> {
    info_field(info, "redis_version")
}

/// The verdict from `INFO memory`'s `maxmemory` and `maxmemory_policy`.
///
/// A `maxmemory` of 0 is no limit, so no policy ever fires. A section missing
/// either line, or reporting an empty one, is unreadable rather than a guess.
fn eviction_from_info(info: &str) -> Eviction {
    let limit = info_field(info, "maxmemory").map(str::trim);
    let policy = info_field(info, "maxmemory_policy").map(str::trim);
    let (Some(limit), Some(policy)) = (limit, policy) else {
        return Eviction::Unreadable;
    };
    if limit.is_empty() || policy.is_empty() {
        return Eviction::Unreadable;
    }
    if limit == "0" || policy.eq_ignore_ascii_case("noeviction") {
        return Eviction::Never;
    }
    Eviction::Evicts {
        policy: policy.to_owned(),
    }
}

/// The verdict from `CONFIG GET appendonly` and `CONFIG GET save`.
///
/// `save` lists the snapshot schedule as pairs of seconds and changes, and is
/// empty when snapshots are off. The append-only file wins when both are on,
/// because it is the one that loses least.
fn persistence(appendonly: &str, save: &str) -> Persistence {
    if appendonly.trim().eq_ignore_ascii_case("yes") {
        return Persistence::AppendOnlyFile;
    }
    if save.trim().is_empty() {
        return Persistence::Nothing;
    }
    Persistence::Snapshots
}

/// The verdict from `INFO persistence`, for a Redis that refuses `CONFIG`.
///
/// That section says whether the append-only file is on, but not what the
/// snapshot schedule is, so anything short of the file is unreadable rather
/// than a guess.
fn persistence_from_info(info: &str) -> Persistence {
    if info.lines().any(|line| line.trim() == "aof_enabled:1") {
        return Persistence::AppendOnlyFile;
    }
    Persistence::Unreadable
}

#[cfg(test)]
mod tests {
    use super::{
        Eviction, Persistence, RedisInspection, eviction_from_info, persistence,
        persistence_from_info, reported_version,
    };

    /// The snapshot schedule a stock `redis-server` runs with.
    const STOCK_SAVE: &str = "3600 1 300 100 60 10000";

    fn reporting(version: Option<&str>) -> RedisInspection {
        RedisInspection {
            version: version.map(str::to_owned),
            eviction: Eviction::Never,
            persistence: Persistence::AppendOnlyFile,
        }
    }

    #[test]
    fn the_version_is_read_off_info_server_and_judged_by_its_major() {
        let info = "# Server\r\nredis_version:7.4.10\r\nredis_mode:standalone\r\n";
        assert_eq!(reported_version(info), Some("7.4.10"));
        assert_eq!(reported_version("# Server\r\n"), None);

        assert_eq!(reporting(Some("7.0.0")).supports_version(), Some(true));
        assert_eq!(reporting(Some("8.0.2")).supports_version(), Some(true));
        assert_eq!(reporting(Some("6.2.14")).supports_version(), Some(false));
        assert_eq!(reporting(Some("unknown")).supports_version(), None);
        assert_eq!(reporting(None).supports_version(), None);
    }

    /// An `INFO memory` section with the two lines that matter, beside the
    /// human-readable one that shares `maxmemory`'s prefix and must not be
    /// mistaken for it.
    fn memory(limit: &str, policy: &str) -> String {
        format!(
            "# Memory\r\nused_memory:1024\r\nmaxmemory_human:4.00M\r\nmaxmemory:{limit}\r\n\
             maxmemory_policy:{policy}\r\n"
        )
    }

    #[test]
    fn only_a_memory_limit_with_an_evicting_policy_evicts() {
        assert_eq!(
            eviction_from_info(&memory("0", "allkeys-lru")),
            Eviction::Never
        );
        assert_eq!(
            eviction_from_info(&memory("4194304", "noeviction")),
            Eviction::Never,
        );
        assert_eq!(
            eviction_from_info(&memory("4194304", "volatile-lru")),
            Eviction::Evicts {
                policy: "volatile-lru".to_owned(),
            },
        );
    }

    #[test]
    fn a_memory_section_that_does_not_say_is_unreadable_not_evicting() {
        assert_eq!(eviction_from_info("# Memory\r\n"), Eviction::Unreadable);
        assert_eq!(eviction_from_info(&memory("", "")), Eviction::Unreadable);
        assert_eq!(
            eviction_from_info("# Memory\r\nmaxmemory:0\r\n"),
            Eviction::Unreadable,
        );
    }

    #[test]
    fn an_append_only_file_persists_whatever_the_snapshots_do() {
        assert_eq!(persistence("yes", ""), Persistence::AppendOnlyFile);
        assert_eq!(persistence("yes", STOCK_SAVE), Persistence::AppendOnlyFile);
        assert_eq!(persistence(" YES ", ""), Persistence::AppendOnlyFile);
    }

    #[test]
    fn snapshot_points_alone_are_snapshots() {
        assert_eq!(persistence("no", STOCK_SAVE), Persistence::Snapshots);
    }

    #[test]
    fn neither_an_append_only_file_nor_snapshot_points_persists_nothing() {
        assert_eq!(persistence("no", ""), Persistence::Nothing);
        assert_eq!(persistence("no", "   "), Persistence::Nothing);
    }

    #[test]
    fn info_confirms_an_append_only_file_and_nothing_less() {
        let on = "# Persistence\r\nloading:0\r\naof_enabled:1\r\naof_rewrite_in_progress:0\r\n";
        let off = "# Persistence\r\nloading:0\r\naof_enabled:0\r\n";

        assert_eq!(persistence_from_info(on), Persistence::AppendOnlyFile);
        // INFO never names the snapshot schedule, so an off file is not proof
        // of nothing persisting.
        assert_eq!(persistence_from_info(off), Persistence::Unreadable);
        assert_eq!(persistence_from_info(""), Persistence::Unreadable);
    }
}
