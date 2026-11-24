//! `anubis doctor`: verify toolchain, database, and config health.
//!
//! Presence checks cover the tools development actually needs (`rustc`,
//! `cargo`, `node`, `yarn`, and optionally `docker`). The database check goes
//! beyond reachability: it establishes a real Postgres connection with the
//! configured `DATABASE_URL`, so bad credentials surface here instead of at
//! first boot. `ANUBIS_SECRET_KEY` is parsed with the same parser
//! configuration uses, so a key that would fail a production boot fails here.
//! Run from an application root, doctor also validates `config/roles.yml` and
//! `config/billing.yml` with the same parsers the server boots with.
//!
//! The session store check names where sessions are kept. When `SESSION_STORE`
//! puts them in Redis, it also asks that Redis how it persists, because a
//! Redis that persists nothing signs every account out when it restarts: an
//! append-only file is what a session store wants, RDB snapshots lose the
//! sign-ins since the last one, and neither is a warning.

use std::collections::HashMap;
use std::process::{Command, ExitCode};
use std::time::Duration;

use anubis::auth::secret_box::SecretKey;
use anubis::config::SessionStore;

/// How long the database gets to accept a connection before doctor gives up.
/// Long enough for a cold local Postgres, short enough to not feel hung.
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long Redis gets to answer every question doctor asks it, together.
/// The same bound as the database, for the same reason.
const REDIS_TIMEOUT: Duration = Duration::from_secs(5);

/// The variable holding the key that encrypts secrets at rest.
const SECRET_KEY_VAR: &str = "ANUBIS_SECRET_KEY";

/// The variable choosing where sessions are kept.
const SESSION_STORE_VAR: &str = "SESSION_STORE";

/// The variable naming the Redis a Redis session store lives in.
const REDIS_URL_VAR: &str = "REDIS_URL";

enum Status {
    Ok,
    Warn,
    Fail,
}

struct Check {
    status: Status,
    detail: String,
}

/// Runs every check and renders the report; fails when development is blocked.
pub(crate) fn run() -> ExitCode {
    let mut checks = tool_checks();
    checks.push(database_check());
    checks.push(session_store_check());
    checks.push(secret_key_check());
    checks.push(roles_check());
    checks.push(billing_check());

    let mut failed = false;
    for check in &checks {
        let label = match check.status {
            Status::Ok => "[ ok ]",
            Status::Warn => "[warn]",
            Status::Fail => {
                failed = true;
                "[FAIL]"
            }
        };
        println!("{label} {}", check.detail);
    }

    if failed {
        eprintln!();
        eprintln!("doctor found blocking problems.");
        return ExitCode::FAILURE;
    }
    println!();
    println!("All good.");
    ExitCode::SUCCESS
}

/// The development toolchain: the four tools nothing works without, and
/// docker, which only the development datastores need.
fn tool_checks() -> Vec<Check> {
    let mut checks = [
        ("rustc", "compiles the backend"),
        ("cargo", "builds and runs the backend"),
        ("node", "runs the frontend toolchain"),
        ("yarn", "manages frontend dependencies"),
    ]
    .into_iter()
    .map(|(program, purpose)| match version_output(program) {
        Some(version) => Check {
            status: Status::Ok,
            detail: version,
        },
        None => Check {
            status: Status::Fail,
            detail: format!("{program} not found on PATH; it {purpose}"),
        },
    })
    .collect::<Vec<_>>();

    checks.push(match version_output("docker") {
        Some(version) => Check {
            status: Status::Ok,
            detail: version,
        },
        None => Check {
            status: Status::Warn,
            detail: "docker not found; `yarn dev` uses it for the development Postgres".to_owned(),
        },
    });
    checks
}

/// One real connection with the configured `DATABASE_URL`.
fn database_check() -> Check {
    match std::env::var("DATABASE_URL") {
        Ok(url) => match connect_database(&url) {
            Ok(()) => Check {
                status: Status::Ok,
                detail: "database connection established".to_owned(),
            },
            Err(reason) => Check {
                status: Status::Fail,
                detail: format!("database connection failed: {reason}"),
            },
        },
        Err(_) => Check {
            status: Status::Warn,
            detail: "DATABASE_URL not set; `yarn dev` provides it for the dev database".to_owned(),
        },
    }
}

/// Where sessions are kept, read with the parser the server boots with.
///
/// A value the server would refuse fails here, and so does a Redis store with
/// no reachable Redis, because either one stops the boot.
fn session_store_check() -> Check {
    let named = std::env::var(SESSION_STORE_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let store = match named.map(|value| value.parse::<SessionStore>()) {
        None => SessionStore::default(),
        Some(Ok(store)) => store,
        Some(Err(error)) => {
            return Check {
                status: Status::Fail,
                detail: error.to_string(),
            };
        }
    };

    match store {
        SessionStore::Postgres => Check {
            status: Status::Ok,
            detail: "sessions are kept in Postgres".to_owned(),
        },
        SessionStore::Redis => redis_sessions_check(),
    }
}

/// How the Redis that holds the sessions keeps them across a restart.
fn redis_sessions_check() -> Check {
    let Some(url) = std::env::var(REDIS_URL_VAR)
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        return Check {
            status: Status::Fail,
            detail: format!(
                "{SESSION_STORE_VAR} is redis, and {REDIS_URL_VAR} is not set; the server refuses \
                 to boot without the Redis to keep sessions in"
            ),
        };
    };

    let report = match read_redis(url.trim()) {
        Ok(report) => report,
        Err(reason) => {
            return Check {
                status: Status::Fail,
                detail: format!(
                    "sessions are kept in Redis, and {REDIS_URL_VAR} did not answer: {reason}"
                ),
            };
        }
    };

    if let Some(version) = report.version.as_deref().filter(|version| too_old(version)) {
        return Check {
            status: Status::Fail,
            detail: format!(
                "sessions are kept in Redis {version}, older than the 7.0 the session store \
                 needs; the server refuses to boot on it"
            ),
        };
    }

    match report.persistence {
        Persistence::AppendOnlyFile => Check {
            status: Status::Ok,
            detail: "sessions are kept in Redis, which persists them in its append-only file"
                .to_owned(),
        },
        Persistence::Snapshots => Check {
            status: Status::Ok,
            detail: "sessions are kept in Redis, which persists them in RDB snapshots; a restart \
                     loses the sign-ins since the last one, and `appendonly yes` keeps every one"
                .to_owned(),
        },
        Persistence::Nothing => Check {
            status: Status::Warn,
            detail: "sessions are kept in Redis, which persists nothing (appendonly is off and \
                     save is empty): a Redis restart signs everyone out. Set `appendonly yes`"
                .to_owned(),
        },
        Persistence::Unreadable => Check {
            status: Status::Warn,
            detail: "sessions are kept in Redis, whose persistence could not be read (it refused \
                     CONFIG GET, as managed Redis often does); confirm it persists, because a \
                     restart without persistence signs everyone out"
                .to_owned(),
        },
    }
}

/// What doctor learned from the Redis that holds the sessions.
struct RedisReport {
    /// The version `INFO server` reported, when it reported one.
    version: Option<String>,
    persistence: Persistence,
}

/// The oldest Redis major version the session store runs on.
///
/// The store's commands need 7.0, and the server refuses to boot on anything
/// older; see `anubis::auth::session`.
const MINIMUM_REDIS_MAJOR: u32 = 7;

/// The version `INFO server` reports, when it reports one.
fn reported_version(info: &str) -> Option<&str> {
    info.lines()
        .find_map(|line| line.trim().strip_prefix("redis_version:"))
}

/// Returns `true` for a version the session store cannot run on.
///
/// A version whose major does not parse is not judged, as the server's boot
/// does not judge it either.
fn too_old(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major < MINIMUM_REDIS_MAJOR)
}

/// What a Redis does to keep its data across a restart.
#[derive(Debug, PartialEq, Eq)]
enum Persistence {
    /// It appends every write to a file it replays on start.
    AppendOnlyFile,
    /// It snapshots on a schedule, losing what changed since the last one.
    Snapshots,
    /// It keeps nothing: a restart starts empty.
    Nothing,
    /// It would not say.
    Unreadable,
}

/// The verdict from `CONFIG GET appendonly` and `CONFIG GET save`.
///
/// `save` lists the snapshot schedule as pairs of seconds and changes, and is
/// empty when snapshots are off. The append-only file wins when both are on,
/// because it is the one that loses nothing.
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

/// Asks the Redis at `url` its version and how it persists, bounded by
/// [`REDIS_TIMEOUT`].
///
/// The URL may carry a password, so no error here ever quotes it.
fn read_redis(url: &str) -> Result<RedisReport, String> {
    let client = redis::Client::open(url)
        .map_err(|_error| "it is not a redis:// or rediss:// connection URL".to_owned())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start an async runtime: {error}"))?;

    runtime.block_on(async {
        let asked = async {
            let mut connection = client.get_multiplexed_async_connection().await?;
            let server: String = redis::cmd("INFO")
                .arg("server")
                .query_async(&mut connection)
                .await?;
            let version = reported_version(&server).map(str::to_owned);

            let appendonly = config_value(&mut connection, "appendonly").await;
            let save = config_value(&mut connection, "save").await;
            let persistence = if let (Ok(appendonly), Ok(save)) = (appendonly, save) {
                persistence(&appendonly, &save)
            } else {
                let info: String = redis::cmd("INFO")
                    .arg("persistence")
                    .query_async(&mut connection)
                    .await?;
                persistence_from_info(&info)
            };

            Ok::<_, redis::RedisError>(RedisReport {
                version,
                persistence,
            })
        };

        match tokio::time::timeout(REDIS_TIMEOUT, asked).await {
            Err(_elapsed) => Err(format!("timed out after {}s", REDIS_TIMEOUT.as_secs())),
            Ok(Err(error)) => Err(error.to_string()),
            Ok(Ok(report)) => Ok(report),
        }
    })
}

/// One setting's value from `CONFIG GET`, empty when Redis has no such setting.
async fn config_value(
    connection: &mut redis::aio::MultiplexedConnection,
    name: &str,
) -> Result<String, redis::RedisError> {
    let mut settings: HashMap<String, String> = redis::cmd("CONFIG")
        .arg("GET")
        .arg(name)
        .query_async(connection)
        .await?;
    Ok(settings.remove(name).unwrap_or_default())
}

/// The key that encrypts secrets at rest, parsed exactly as boot parses it.
///
/// A malformed key is a production boot failure waiting to happen, so it
/// fails here. An absent one is the zero-configuration development path, so
/// it warns and names the command that ends it.
fn secret_key_check() -> Check {
    match std::env::var(SECRET_KEY_VAR) {
        Ok(encoded) => match SecretKey::from_base64(&encoded) {
            Ok(_key) => Check {
                status: Status::Ok,
                detail: format!("{SECRET_KEY_VAR} is a valid 32-byte key"),
            },
            Err(error) => Check {
                status: Status::Fail,
                detail: format!("{SECRET_KEY_VAR} is malformed: {error}"),
            },
        },
        Err(_) => Check {
            status: Status::Warn,
            detail: format!(
                "{SECRET_KEY_VAR} not set; secrets at rest use the public development key. \
                 Generate one with `anubis secret generate`"
            ),
        },
    }
}

/// The application's `roles.yml`, parsed by the parser the server boots with.
fn roles_check() -> Check {
    match std::fs::read_to_string("config/roles.yml") {
        Ok(yaml) => match anubis::roles::RoleSet::from_yaml(&yaml) {
            Ok(set) => Check {
                status: Status::Ok,
                detail: format!(
                    "config/roles.yml is valid ({} roles)",
                    set.role_keys().count()
                ),
            },
            Err(error) => Check {
                status: Status::Fail,
                detail: format!("config/roles.yml is invalid: {error}"),
            },
        },
        Err(_) => Check {
            status: Status::Warn,
            detail: "config/roles.yml not found; run doctor from an application root".to_owned(),
        },
    }
}

/// The application's `config/billing.yml`, parsed the way the server parses it.
///
/// Absent is a normal state: an application that charges for nothing has no
/// plans file, and the framework needs none.
fn billing_check() -> Check {
    let Ok(yaml) = std::fs::read_to_string("config/billing.yml") else {
        return Check {
            status: Status::Ok,
            detail: "config/billing.yml not found; billing is off for this application".to_owned(),
        };
    };

    match anubis::billing::PlanSet::from_yaml(&yaml) {
        Ok(plans) => Check {
            status: Status::Ok,
            detail: format!(
                "config/billing.yml is valid ({} plans, free plan {:?})",
                plans.plans().len(),
                plans.free().key(),
            ),
        },
        Err(error) => Check {
            status: Status::Fail,
            detail: format!("config/billing.yml is invalid: {error}"),
        },
    }
}

/// The first `--version` line of `program`, when it runs successfully.
///
/// Windows resolves commands through `cmd` so `.cmd` shims (yarn, node
/// version managers) are found the same way a terminal finds them.
fn version_output(program: &str) -> Option<String> {
    let output = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", program, "--version"])
            .output()
    } else {
        Command::new(program).arg("--version").output()
    };
    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    // Bare version numbers (node prints `v22.1.0`) read better with the name.
    if line.to_ascii_lowercase().contains(program) {
        Some(line.to_owned())
    } else {
        Some(format!("{program} {line}"))
    }
}

/// Establishes one real Postgres connection, bounded by [`DATABASE_TIMEOUT`].
fn connect_database(url: &str) -> Result<(), String> {
    use diesel_async::{AsyncConnection, AsyncPgConnection};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start an async runtime: {error}"))?;

    runtime.block_on(async {
        match tokio::time::timeout(DATABASE_TIMEOUT, AsyncPgConnection::establish(url)).await {
            Err(_) => Err(format!("timed out after {}s", DATABASE_TIMEOUT.as_secs())),
            Ok(Err(error)) => Err(error.to_string()),
            Ok(Ok(_)) => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{Persistence, persistence, persistence_from_info, reported_version, too_old};

    #[test]
    fn the_version_is_read_off_info_server_and_judged_by_its_major() {
        let info = "# Server\r\nredis_version:7.4.10\r\nredis_mode:standalone\r\n";
        assert_eq!(reported_version(info), Some("7.4.10"));
        assert_eq!(reported_version("# Server\r\n"), None);

        assert!(!too_old("7.0.0"));
        assert!(!too_old("8.0.2"));
        assert!(too_old("6.2.14"));
        // Unreadable is not judged, which is what the server's boot does too.
        assert!(!too_old("unknown"));
    }

    /// The snapshot schedule a stock `redis-server` runs with.
    const STOCK_SAVE: &str = "3600 1 300 100 60 10000";

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
