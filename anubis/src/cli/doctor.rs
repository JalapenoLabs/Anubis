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

use std::process::{Command, ExitCode};
use std::time::Duration;

use anubis::auth::secret_box::SecretKey;

/// How long the database gets to accept a connection before doctor gives up.
/// Long enough for a cold local Postgres, short enough to not feel hung.
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

/// The variable holding the key that encrypts secrets at rest.
const SECRET_KEY_VAR: &str = "ANUBIS_SECRET_KEY";

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
