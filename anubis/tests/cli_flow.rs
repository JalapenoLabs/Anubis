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
    for expected in ["Roles drift check", "Client drift check"] {
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
