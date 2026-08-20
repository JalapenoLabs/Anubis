//! What the published crate must carry, and what would silently fall out of it.
//!
//! `cargo package` copies only files under `anubis/`, and only the ones the
//! `include` list in Cargo.toml names, because that list is what makes cargo
//! walk the filesystem instead of asking git (the starter template is vendored
//! in as a gitignored build artifact, so asking git would find nothing). Both
//! halves of that arrangement fail quietly: an unlisted directory and a pruned
//! one each produce a crate that builds and then cannot stamp an application.
//! These tests fail loudly instead, in the monorepo, before a release.
//!
//! See `scripts/vendor-starter.sh` and `build.rs` for the other half.

use std::path::{Path, PathBuf};

/// The package directory, which is where all of this is relative to.
fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The top-level names the `include` list covers, with any glob suffix cut off.
///
/// Reading the manifest as text rather than through a TOML parser keeps this
/// test free of a dependency whose only user would be this test.
fn included_names() -> Vec<String> {
    let manifest = std::fs::read_to_string(package_dir().join("Cargo.toml"))
        .expect("the package manifest is readable");
    let list = manifest
        .split_once("include = [")
        .expect("the manifest has an include list")
        .1
        .split_once(']')
        .expect("the include list is closed")
        .0;
    list.split(',')
        .filter_map(|entry| {
            let entry = entry.trim().trim_matches('"');
            if entry.is_empty() {
                return None;
            }
            Some(
                entry
                    .split_once('/')
                    .map_or_else(|| entry.to_owned(), |(name, _)| name.to_owned()),
            )
        })
        .collect()
}

#[test]
fn every_package_directory_is_published_or_deliberately_not() {
    // What lives under `anubis/` without belonging in the crate: the tests,
    // which drive the monorepo's layout and a real Postgres, and cargo's own
    // build directory when one has been made here.
    let unpublished = ["tests", "target"];
    let included = included_names();

    for entry in std::fs::read_dir(package_dir()).expect("the package directory is readable") {
        let entry = entry.expect("the package directory lists");
        let name = entry.file_name();
        let name = name.to_str().expect("package paths are UTF-8");
        // Cargo always packages the manifest, whatever `include` says.
        if name == "Cargo.toml" || unpublished.contains(&name) {
            continue;
        }
        assert!(
            included.iter().any(|entry| entry == name),
            "`{name}` is under anubis/ but no `include` entry in Cargo.toml \
             names it, so the published crate would not carry it. Add it to \
             the list, or add it to this test's `unpublished` names."
        );
    }
}

#[test]
fn no_published_directory_is_pruned_as_a_nested_package() {
    let package_dir = package_dir();
    for name in included_names() {
        // The vendored starter is the one tree that does carry manifests, and
        // scripts/vendor-starter.sh renames them for exactly this reason. It
        // exists only after vendoring, so there is usually nothing to walk.
        if name == "starter" {
            continue;
        }
        let path = package_dir.join(&name);
        if path.is_dir() {
            assert_no_manifest_below(&path);
        }
    }
}

/// Fails if any directory under `directory` holds a `Cargo.toml`.
///
/// Cargo's packager takes such a directory for a nested package and prunes it,
/// dropping every file below it from the crate without saying so.
fn assert_no_manifest_below(directory: &Path) {
    for entry in std::fs::read_dir(directory).expect("an included directory is readable") {
        let path = entry.expect("an included directory lists").path();
        if path.is_dir() {
            assert_no_manifest_below(&path);
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            panic!(
                "{} would make cargo prune {} from the published crate as a \
                 nested package. Store it under another name, as \
                 templates/new/workspace-Cargo.toml is.",
                path.display(),
                directory.display()
            );
        }
    }
}
