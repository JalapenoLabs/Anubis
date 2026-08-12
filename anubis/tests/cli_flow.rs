//! One linear narrative through the CLI: stamp an app, inspect the routes.
//!
//! Runs the real `anubis` binary (cargo builds and exposes it via
//! `CARGO_BIN_EXE_anubis`), so this covers argument parsing, the embedded
//! starter template, and terminal output end to end. Needs no database.

use std::path::Path;
use std::process::Command;

const ANUBIS: &str = env!("CARGO_BIN_EXE_anubis");

#[test]
fn new_stamps_a_complete_renamed_application() {
    let scratch = std::env::temp_dir().join(format!("anubis-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory is creatable");

    let output = Command::new(ANUBIS)
        .args(["new", "acme-crm"])
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
        stdout.contains("created `acme-crm`"),
        "unexpected output: {stdout}"
    );

    let app = scratch.join("acme-crm");
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
    let rerun = Command::new(ANUBIS)
        .args(["new", "acme-crm"])
        .current_dir(&scratch)
        .output()
        .expect("the anubis binary runs");
    assert!(
        !rerun.status.success(),
        "anubis new must refuse a non-empty target"
    );

    std::fs::remove_dir_all(&scratch).expect("scratch directory is removable");
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
