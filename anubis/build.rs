//! Embeds the starter template into the `anubis` binary.
//!
//! `anubis new` stamps a complete application from the `starter/` tree.
//! Embedding it at build time keeps `anubis new` offline and guarantees the
//! stamped template always matches the installed framework version. The
//! generated `starter_files.rs` is included by the CLI's `new` module.
//!
//! The template lives in one of two places, and this script takes whichever it
//! finds. A published crate carries its own copy at `anubis/starter`, put
//! there by `scripts/vendor-starter.sh` before `cargo package`, because
//! `cargo package` reaches nothing above the package directory. In the
//! monorepo there is no vendored copy and the template is `../starter`, the
//! living tree CI scaffolds into. Preferring the vendored copy means a
//! developer who vendors builds exactly what a consumer of the crate builds.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory names never embedded: build outputs, test artifacts, and
/// dependency caches that may exist locally but are not part of the template.
const SKIPPED_DIRECTORIES: [&str; 8] = [
    "node_modules",
    "target",
    "dist",
    "coverage",
    ".vite",
    ".git",
    // What a local `yarn test:e2e` leaves behind: the HTML report and the
    // traces, screenshots, and videos of failed runs.
    "playwright-report",
    "test-results",
];

/// The suffix `scripts/vendor-starter.sh` appends to the template's own
/// `Cargo.toml`, and this script strips back off.
///
/// Cargo's packager prunes any directory holding a `Cargo.toml`, taking it for
/// a nested package, which would drop the starter's whole backend from the
/// published crate. The manifest therefore travels under a name cargo has no
/// opinion about, and the stamped application still receives `Cargo.toml`.
/// Nothing in the monorepo tree carries the suffix, so the strip is a no-op
/// there.
const VENDORED_SUFFIX: &str = ".vendored";

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("starter");
    let monorepo_dir = manifest_dir
        .parent()
        .expect("the anubis package lives inside the repository")
        .join("starter");

    // Both paths are watched, not just the one in use: vendoring or cleaning
    // the copy changes which tree is embedded, and that must rebuild.
    println!("cargo:rerun-if-changed={}", vendored_dir.display());
    println!("cargo:rerun-if-changed={}", monorepo_dir.display());

    let starter_dir = if vendored_dir.is_dir() {
        vendored_dir
    } else {
        monorepo_dir
    };
    assert!(
        starter_dir.is_dir(),
        "no starter template at {}: a published crate carries one, and the \
         monorepo has one at the repository root",
        starter_dir.display()
    );

    let mut files = Vec::new();
    collect_files(&starter_dir, &starter_dir, &mut files);
    files.sort();

    let mut generated = String::from(
        "/// Every starter-template file, as `(relative path, contents)`.\n\
         pub(crate) static STARTER_FILES: &[(&str, &[u8])] = &[\n",
    );
    for (relative, absolute) in &files {
        let absolute = absolute.to_str().expect("starter paths are UTF-8");
        writeln!(
            generated,
            "    ({relative:?}, include_bytes!({absolute:?})),"
        )
        .expect("writing to a String never fails");
    }
    generated.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out_dir.join("starter_files.rs"), generated)
        .expect("failed to write the generated starter file list");
}

/// Collects every embeddable file under `directory` as
/// `(forward-slash relative path, absolute path)`.
fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("failed to read a starter directory entry");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_str().expect("starter file names are UTF-8");
        if path.is_dir() {
            if !SKIPPED_DIRECTORIES.contains(&name) {
                collect_files(root, &path, files);
            }
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("children live under the root")
                .to_str()
                .expect("starter paths are UTF-8")
                .replace('\\', "/");
            let relative = match relative.strip_suffix(VENDORED_SUFFIX) {
                Some(stripped) => stripped.to_owned(),
                None => relative,
            };
            files.push((relative, path.clone()));
        }
    }
}
