//! CLI command implementations for the `anubis` binary.
//!
//! The library's [`anubis::scaffold`](anubis::scaffold) module holds the pure
//! text-transformation engine; these modules are its thin I/O shell: they
//! discover files, read and write the filesystem, probe the environment, and
//! render terminal output. Keeping side effects here keeps the engine
//! trivially unit-testable and the binary honest about what touches disk.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub(crate) mod doctor;
pub(crate) mod eject;
pub(crate) mod new;
pub(crate) mod routes;
pub(crate) mod scaffold;
pub(crate) mod secret;
pub(crate) mod upgrade;

/// The Rust edition the framework and every stamped application build on.
const EDITION: &str = "2024";

/// Formats the Rust files among `written` with `rustfmt`, when it is on `PATH`.
///
/// Every command here writes Rust by name-for-name transformation, and a
/// transformation preserves neither import order nor line width: a name that
/// sorts elsewhere moves within its `use` block, a longer one pushes a
/// statement past the width, a shorter one lets a wrapped statement fit again.
/// Where a name lands depends on the name the developer typed, so no template
/// can be ordered correctly for all of them and the formatter is what settles
/// it. That is what keeps `cargo fmt --check` green on code nobody has touched.
///
/// A missing `rustfmt` is not fatal, because the output is valid Rust either
/// way; the command says what to run instead and carries on.
pub(crate) fn format_rust_files(written: &[PathBuf]) {
    let files = written
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return;
    }

    match Command::new("rustfmt")
        .args(["--edition", EDITION])
        .args(&files)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(_status) => {
            eprintln!("warning: rustfmt reported an error; run `cargo fmt --all` to format it");
        }
        Err(_error) => {
            eprintln!(
                "warning: rustfmt is not on PATH; run `cargo fmt --all` to format the output"
            );
        }
    }
}

/// The application root the command was run in.
///
/// An application root holds `backend/`, `frontend/`, and `config/roles.yml`.
/// The search walks up from the working directory, so the command works from
/// anywhere inside an application; inside this repository it finds `starter/`,
/// the template host app.
pub(crate) fn app_root() -> Result<PathBuf, String> {
    let working_directory = std::env::current_dir()
        .map_err(|error| format!("failed to read the working directory: {error}"))?;

    working_directory
        .ancestors()
        .find(|candidate| is_app_root(candidate))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "{} is not inside an Anubis application; run this from a directory holding \
                 backend/, frontend/, and config/roles.yml (in this repository that is \
                 starter/)",
                working_directory.display(),
            )
        })
}

fn is_app_root(path: &Path) -> bool {
    path.join("backend/src").is_dir()
        && path.join("frontend").is_dir()
        && path.join("config/roles.yml").is_file()
}

pub(crate) fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

/// Renders a relative path with forward slashes, the way the docs write them.
pub(crate) fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn fail(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    ExitCode::FAILURE
}
