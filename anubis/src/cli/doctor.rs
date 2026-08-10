//! `anubis doctor`: verify toolchain, database, and config health.
//!
//! Presence checks cover the tools development actually needs (`rustc`,
//! `cargo`, `node`, `yarn`, and optionally `docker`). The database check goes
//! beyond reachability: it establishes a real Postgres connection with the
//! configured `DATABASE_URL`, so bad credentials surface here instead of at
//! first boot. Run from an application root, it also validates
//! `config/roles.yml` with the same parser the server boots with.

use std::process::{Command, ExitCode};
use std::time::Duration;

/// How long the database gets to accept a connection before doctor gives up.
/// Long enough for a cold local Postgres, short enough to not feel hung.
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

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
    let mut checks = Vec::new();

    for (program, purpose) in [
        ("rustc", "compiles the backend"),
        ("cargo", "builds and runs the backend"),
        ("node", "runs the frontend toolchain"),
        ("yarn", "manages frontend dependencies"),
    ] {
        checks.push(match version_output(program) {
            Some(version) => Check {
                status: Status::Ok,
                detail: version,
            },
            None => Check {
                status: Status::Fail,
                detail: format!("{program} not found on PATH; it {purpose}"),
            },
        });
    }

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

    checks.push(match std::env::var("DATABASE_URL") {
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
    });

    checks.push(match std::fs::read_to_string("config/roles.yml") {
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
    });

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
