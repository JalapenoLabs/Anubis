//! Embeds the starter template into the `anubis` binary.
//!
//! `anubis new` stamps a complete application from the `starter/` tree at the
//! repository root. Embedding it at build time keeps `anubis new` offline and
//! guarantees the stamped template always matches the installed framework
//! version. The generated `starter_files.rs` is included by the CLI's `new`
//! module.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory names never embedded: build outputs and dependency caches that
/// may exist locally but are not part of the template.
const SKIPPED_DIRECTORIES: [&str; 6] = [
    "node_modules",
    "target",
    "dist",
    "coverage",
    ".vite",
    ".git",
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let starter_dir = manifest_dir
        .parent()
        .expect("the anubis package lives inside the repository")
        .join("starter");
    println!("cargo:rerun-if-changed={}", starter_dir.display());

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
            files.push((relative, path.clone()));
        }
    }
}
