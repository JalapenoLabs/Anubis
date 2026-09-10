//! One linear narrative through the CLI: stamp an app, inspect the routes.
//!
//! Runs the real `anubis` binary (cargo builds and exposes it via
//! `CARGO_BIN_EXE_anubis`), so this covers argument parsing, the embedded
//! starter template, and terminal output end to end. Needs no database.

use std::path::{Path, PathBuf};
use std::process::Command;

const ANUBIS: &str = env!("CARGO_BIN_EXE_anubis");

/// Stamps `name` into a scratch directory of its own, returning the app root.
///
/// Each test stamps a distinct name, so scratch trees never collide and the
/// tests still run in parallel. Callers remove the parent when they are done.
fn stamp(name: &str, arguments: &[&str]) -> PathBuf {
    let scratch =
        std::env::temp_dir().join(format!("anubis-cli-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory is creatable");

    let output = Command::new(ANUBIS)
        .args(["new", name])
        .args(arguments)
        .current_dir(&scratch)
        .output()
        .expect("the anubis binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "anubis new failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains(&format!("created `{name}`")),
        "unexpected output: {stdout}"
    );

    scratch.join(name)
}

#[test]
fn new_stamps_a_complete_renamed_application() {
    let app = stamp("acme-crm", &[]);
    for expected in [
        "Cargo.toml",
        "backend/Cargo.toml",
        "backend/src/main.rs",
        "frontend/package.json",
        "frontend/src/main.tsx",
        "frontend/playwright.config.ts",
        "frontend/e2e/creative-concepts.spec.ts",
        "config/roles.yml",
        "compose.yaml",
        "package.json",
        ".yarnrc.yml",
        ".gitignore",
        "README.md",
        "rust-toolchain.toml",
        "clippy.toml",
        ".github/workflows/ci.yml",
        ".env.example",
    ] {
        assert!(
            app.join(expected).is_file(),
            "stamped app is missing {expected}"
        );
    }

    // The app is renamed throughout: manifests carry the new name and no
    // starter token survives anywhere in the tree.
    let backend_manifest = read(&app.join("backend/Cargo.toml"));
    assert!(backend_manifest.contains("name = \"acme-crm\""));
    // The framework publishes as `anubis-framework` and the manifest renames
    // it back, which is what keeps every `use anubis::` in the app compiling.
    assert!(
        backend_manifest.contains("anubis = { package = \"anubis-framework\","),
        "the backend manifest must rename the framework crate: {backend_manifest}"
    );
    let frontend_manifest = read(&app.join("frontend/package.json"));
    assert!(frontend_manifest.contains("\"name\": \"acme-crm-frontend\""));
    assert!(frontend_manifest.contains("#workspace=@jalapenolabs/anubis"));
    let index_html = read(&app.join("frontend/index.html"));
    assert!(
        index_html.contains("<title>Acme Crm</title>"),
        "title not stamped: {index_html}"
    );
    assert_no_token(&app);

    // Stamping refuses to clobber the now non-empty directory.
    let scratch = app.parent().expect("the app sits inside its scratch tree");
    let rerun = Command::new(ANUBIS)
        .args(["new", "acme-crm"])
        .current_dir(scratch)
        .output()
        .expect("the anubis binary runs");
    assert!(
        !rerun.status.success(),
        "anubis new must refuse a non-empty target"
    );

    std::fs::remove_dir_all(scratch).expect("scratch directory is removable");
}

/// A stamped tree passes the `cargo fmt --check` its own CI runs first.
///
/// The name is the point. Templates carry the import order their own crate
/// name earned, and stamping moves the name within every `use` block it
/// appears in: `zebra` sorts after `axum`, `acme` sorts before it. Nothing
/// but the formatter can be right for both, so `anubis new` runs it.
#[test]
fn new_stamps_a_formatted_tree() {
    let app = stamp("zebra", &[]);

    let mut sources = Vec::new();
    collect_rust_files(&app, &mut sources);
    assert!(!sources.is_empty(), "a stamped app ships Rust sources");

    let check = Command::new("rustfmt")
        .args(["--edition", "2024", "--check"])
        .args(&sources)
        .output()
        .expect("rustfmt is on PATH wherever the framework is built");
    assert!(
        check.status.success(),
        "the stamped tree is unformatted:\n{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr),
    );

    remove(&app);
}

/// A stamped app arrives with CI and its development environment in place,
/// and with no credential written twice.
#[test]
fn new_stamps_ci_and_development_configuration() {
    let app = stamp("acme-books", &[]);

    let environment = read(&app.join(".env.example"));
    assert!(
        environment.contains("POSTGRES_DB=acme_books_development"),
        "the database is not named after the app: {environment}"
    );
    let manifest = read(&app.join("package.json"));
    assert!(
        !manifest.contains("postgres://"),
        "the dev script still hardcodes the database URL: {manifest}"
    );
    assert!(
        manifest.contains("dotenv --"),
        "the dev script does not load .env: {manifest}"
    );

    let workflow = read(&app.join(".github/workflows/ci.yml"));
    assert!(workflow.contains("runs-on: ubuntu-latest"), "{workflow}");
    assert!(
        workflow.contains("POSTGRES_DB: acme_books_test"),
        "{workflow}"
    );
    for expected in ["Roles drift check", "Client drift check", "name: E2E"] {
        assert!(workflow.contains(expected), "CI is missing {expected}");
    }

    remove(&app);
}

/// Applications are private by default, and `--license mit` is the opt out.
#[test]
fn new_licenses_the_application_on_request() {
    let private = stamp("acme-tickets", &[]);
    assert!(
        read(&private.join("package.json")).contains("\"license\": \"UNLICENSED\""),
        "the default manifest must keep the application private"
    );
    assert!(
        !private.join("LICENSE").exists(),
        "an unlicensed app must not carry a LICENSE file"
    );
    remove(&private);

    let licensed = stamp("acme-shop", &["--license", "mit"]);
    let license = read(&licensed.join("LICENSE"));
    assert!(license.starts_with("MIT License"), "{license}");
    assert!(!license.contains("{year}"), "the year is not stamped");
    assert!(
        read(&licensed.join("package.json")).contains("\"license\": \"MIT\""),
        "the manifest does not carry the license"
    );
    remove(&licensed);
}

/// `anubis upgrade` from a stamped application: refused while the framework is
/// tracked from git, then planning both rewrites and every generator once the
/// two dependencies name published versions.
///
/// `--to` is what keeps this offline: a version named on the command line is
/// the whole answer, so the registry is never reached.
#[test]
fn upgrade_plans_a_release_bump_and_refuses_a_git_tracked_application() {
    let app = stamp("acme-parts", &[]);
    let backend = app.join("backend/Cargo.toml");
    let frontend = app.join("frontend/package.json");

    // As stamped, both ends track the framework from git. There is no version
    // to bump, and the command says so rather than inventing one.
    let refused = upgrade(&app, &["--dry-run"]);
    assert!(
        !refused.status.success(),
        "a git-tracked application has nothing to upgrade"
    );
    let reason = String::from_utf8_lossy(&refused.stderr);
    for expected in [
        "backend/Cargo.toml",
        "frontend/package.json",
        "git repository",
        "docs/upgrading.md",
    ] {
        assert!(reason.contains(expected), "unexpected refusal: {reason}");
    }

    // The arrangement every application has once the packages are published.
    // The crate publishes as `anubis-framework`, so the `package` key that
    // renames it back survives the move from git to a version.
    rewrite(
        &backend,
        "anubis = { package = \"anubis-framework\", git = \
         \"https://github.com/JalapenoLabs/Anubis.git\" }",
        "anubis = { package = \"anubis-framework\", version = \"0.1.0\" }",
    );
    rewrite(
        &frontend,
        "\"@jalapenolabs/anubis\": \
         \"https://github.com/JalapenoLabs/Anubis.git#workspace=@jalapenolabs/anubis\"",
        "\"@jalapenolabs/anubis\": \"^0.1.0\"",
    );
    let before = (read(&backend), read(&frontend));

    let planned = upgrade(&app, &["--to", "0.2.0", "--dry-run"]);
    assert!(
        planned.status.success(),
        "upgrade --dry-run failed: {}",
        String::from_utf8_lossy(&planned.stderr),
    );
    let plan = String::from_utf8_lossy(&planned.stdout);
    for expected in [
        "upgrade acme-parts to anubis 0.2.0",
        // Both manifests, each keeping its own range operator.
        "backend/Cargo.toml     0.1.0 -> 0.2.0",
        "frontend/package.json  ^0.1.0 -> ^0.2.0",
        // The two lockfiles, then every generator the application carries.
        "cargo update -p anubis-framework",
        "yarn install",
        "anubis roles generate-ts",
        "anubis billing generate-ts",
        "cargo run --quiet -p acme-parts -- openapi",
        "anubis client generate-ts",
        "--dry-run: nothing was written.",
        "https://github.com/JalapenoLabs/Anubis/releases/tag/v0.2.0",
    ] {
        assert!(
            plan.contains(expected),
            "plan is missing `{expected}`: {plan}"
        );
    }

    // A dry run writes nothing, the manifests least of all.
    assert_eq!(before, (read(&backend), read(&frontend)));

    remove(&app);
}

/// Runs `anubis upgrade` inside `app`.
fn upgrade(app: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(ANUBIS)
        .arg("upgrade")
        .args(arguments)
        .current_dir(app)
        .output()
        .expect("the anubis binary runs")
}

/// Replaces `from` with `to` in the file at `path`, which must contain it.
fn rewrite(path: &Path, from: &str, to: &str) {
    let contents = read(path);
    assert!(
        contents.contains(from),
        "{} no longer contains `{from}`",
        path.display(),
    );
    std::fs::write(path, contents.replace(from, to))
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}

#[test]
fn secret_generate_prints_a_usable_key() {
    let output = Command::new(ANUBIS)
        .args(["secret", "generate"])
        .output()
        .expect("the anubis binary runs");
    assert!(output.status.success());

    let key = String::from_utf8_lossy(&output.stdout);
    // Stdout carries the key alone, so `$(anubis secret generate)` is usable.
    let key = key.trim();
    anubis::auth::secret_box::SecretKey::from_base64(key).expect("the printed key must parse");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ANUBIS_SECRET_KEY"),
        "the guidance must name the variable"
    );
}

#[test]
fn routes_prints_the_framework_and_api_surface() {
    let output = Command::new(ANUBIS)
        .arg("routes")
        .output()
        .expect("the anubis binary runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "/auth/login",
        "/tenancy/memberships",
        "/api/v1/team",
        "/api/v1/openapi.json",
    ] {
        assert!(
            stdout.contains(expected),
            "routes output is missing {expected}: {stdout}"
        );
    }
}

#[test]
fn doctor_renders_a_report() {
    // Doctor's exit code depends on the machine; the report shape does not.
    let output = Command::new(ANUBIS)
        .arg("doctor")
        .env_remove("DATABASE_URL")
        .env_remove("ANUBIS_SECRET_KEY")
        .output()
        .expect("the anubis binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rustc"),
        "doctor output is missing the rustc check: {stdout}"
    );
    assert!(
        stdout.contains("DATABASE_URL not set"),
        "doctor must warn about the unset DATABASE_URL: {stdout}",
    );
    assert!(
        stdout.contains("[warn] ANUBIS_SECRET_KEY not set")
            && stdout.contains("anubis secret generate"),
        "doctor must warn about the development key and name the generator: {stdout}",
    );
}

#[test]
fn doctor_fails_on_a_malformed_secret_key() {
    let output = Command::new(ANUBIS)
        .arg("doctor")
        .env("ANUBIS_SECRET_KEY", "c2hvcnQ=")
        .output()
        .expect("the anubis binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("[FAIL] ANUBIS_SECRET_KEY is malformed"),
        "doctor must fail a key that cannot be used: {stdout}",
    );
    assert!(
        !output.status.success(),
        "a malformed key blocks development"
    );
}

/// Removes the scratch tree a stamped app lives in.
fn remove(app: &Path) {
    let scratch = app.parent().expect("the app sits inside its scratch tree");
    std::fs::remove_dir_all(scratch).expect("scratch directory is removable");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

/// Collects every `.rs` file under `directory` into `into`.
///
/// The walk is what makes the format check a real gate: it finds the files the
/// stamp wrote rather than the files the stamp remembered to format.
fn collect_rust_files(directory: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("stamped directories are readable") {
        let path = entry.expect("stamped entries are readable").path();
        if path.is_dir() {
            collect_rust_files(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

fn assert_no_token(directory: &Path) {
    for entry in std::fs::read_dir(directory).expect("stamped directories are readable") {
        let path = entry.expect("stamped entries are readable").path();
        if path.is_dir() {
            assert_no_token(&path);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            assert!(
                !text.contains("anubis-starter"),
                "{} still contains `anubis-starter`",
                path.display(),
            );
        }
    }
}
