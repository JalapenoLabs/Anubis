//! One narrative through `anubis eject`: catalog, ejection, rewiring, refusal.
//!
//! The command reads the frontend package out of the application's own
//! `node_modules`, so the test builds a scratch application whose
//! `node_modules/@jalapenolabs/anubis` is a copy of this repository's real
//! package. That keeps the assertions honest about the files the package
//! actually ships, and keeps the repository's own tree untouched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANUBIS: &str = env!("CARGO_BIN_EXE_anubis");

/// The consumer the scratch application ships, importing two field components
/// from one line, which is the shape the rewrite has to split.
const CONSUMER: &str = "\
// Copyright © 2026 Jalapeno Labs

import { TextAreaField, TextField } from '@jalapenolabs/anubis'

export function DemoForm() {
  return null
}
";

#[test]
fn list_prints_the_ejectable_surface_and_its_price() {
    let output = run(&std::env::temp_dir(), &["eject", "--list"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "TextField",
        "FieldWrapper",
        "useFieldState",
        "frontend/src/anubis/",
    ] {
        assert!(
            stdout.contains(expected),
            "the catalog is missing {expected}: {stdout}"
        );
    }
    assert!(
        stdout.contains("no longer improves it"),
        "the catalog must state what ownership costs: {stdout}",
    );
    // Behavior is not ejectable, and the catalog says why.
    assert!(!stdout.contains("useCurrentUser"), "{stdout}");
    assert!(stdout.contains("realtime client"), "{stdout}");
}

#[test]
fn an_unknown_component_is_refused_with_the_catalog() {
    let application = application("unknown");
    let output = run(&application, &["eject", "AnubisProvider"]);

    assert!(
        !output.status.success(),
        "an unknown component must not be ejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not an ejectable component"), "{stderr}");
    assert!(stderr.contains("TextField"), "{stderr}");

    remove(&application);
}

/// The whole feature: the copy lands, its imports are rewritten, the
/// application's own import moves to it, and a second run refuses.
#[test]
fn ejecting_a_component_moves_it_and_its_consumers_into_the_application() {
    let application = application("ejected");
    let output = run(&application, &["eject", "TextField"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "eject failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr),
    );

    // The component lands under the mirrored tree, carrying its provenance and
    // the copyright header the frontend's lint bar requires first.
    let ejected = read(&application.join("frontend/src/anubis/fields/TextField.tsx"));
    assert!(
        ejected.starts_with("// Copyright © 2026 Jalapeno Labs\n"),
        "{ejected}"
    );
    assert!(
        ejected.contains("// Ejected from @jalapenolabs/anubis v"),
        "the copy must say where it came from: {ejected}",
    );
    assert!(ejected.contains("src/fields/TextField.tsx"), "{ejected}");

    // A dependency the package exports is read from the package; one it keeps
    // to itself came along, and is still reached where the package put it.
    assert!(
        ejected.contains("import type { AnubisFieldProps } from '@jalapenolabs/anubis'"),
        "{ejected}",
    );
    assert!(
        ejected.contains("import { TextualField } from './internal/TextualField'"),
        "{ejected}",
    );

    let internal = read(&application.join("frontend/src/anubis/fields/internal/TextualField.tsx"));
    assert!(
        internal.contains("import { FieldWrapper, useFieldState } from '@jalapenolabs/anubis'"),
        "the internal copy must read its public dependencies from the package on one line: \
         {internal}",
    );
    assert!(!internal.contains("from '../FieldWrapper'"), "{internal}");

    // The application's own import splits: the ejected name moves to the copy,
    // the rest stays with the package.
    let consumer = read(&application.join("frontend/src/components/DemoForm.tsx"));
    assert_eq!(
        consumer,
        "\
// Copyright © 2026 Jalapeno Labs

import { TextAreaField } from '@jalapenolabs/anubis'
import { TextField } from '../anubis/fields/TextField'

export function DemoForm() {
  return null
}
",
        "{consumer}",
    );

    for expected in [
        "frontend/src/anubis/fields/TextField.tsx",
        "frontend/src/anubis/fields/internal/TextualField.tsx",
        "frontend/src/components/DemoForm.tsx",
    ] {
        assert!(
            stdout.contains(expected),
            "the report is missing {expected}: {stdout}"
        );
    }

    // Ejecting it again refuses, and says where the file it would have written
    // already lives.
    let again = run(&application, &["eject", "TextField"]);
    assert!(!again.status.success(), "a second ejection must refuse");
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("already ejected"), "{stderr}");
    assert!(
        stderr.contains("frontend/src/anubis/fields/TextField.tsx"),
        "{stderr}"
    );

    // A sibling sharing the internal dependency reuses it rather than failing.
    let sibling = run(&application, &["eject", "EmailField"]);
    let stdout = String::from_utf8_lossy(&sibling.stdout);
    assert!(
        sibling.status.success(),
        "a sibling sharing an internal file must eject: {stdout}{}",
        String::from_utf8_lossy(&sibling.stderr),
    );
    assert!(
        stdout.contains("already ejected, so left alone"),
        "{stdout}"
    );
    let email = read(&application.join("frontend/src/anubis/fields/EmailField.tsx"));
    assert!(
        email.contains("import { TextualField } from './internal/TextualField'"),
        "{email}",
    );

    remove(&application);
}

#[test]
fn an_application_without_the_package_installed_is_told_to_install_it() {
    let application = application("uninstalled");
    std::fs::remove_dir_all(application.join("node_modules")).expect("node_modules is removable");

    let output = run(&application, &["eject", "TextField"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("yarn install"), "{stderr}");

    remove(&application);
}

/// Builds a scratch application: the three markers `anubis` looks for, one
/// consumer, and the real frontend package under `node_modules`.
fn application(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("anubis-eject-{}-{name}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("a stale scratch tree is removable");
    }

    create(&root.join("backend/src"));
    create(&root.join("config"));
    create(&root.join("frontend/src/components"));
    write(&root.join("config/roles.yml"), "roles:\n");
    write(&root.join("frontend/src/components/DemoForm.tsx"), CONSUMER);

    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../frontend");
    let installed = root.join("node_modules/@jalapenolabs/anubis");
    create(&installed);
    std::fs::copy(package.join("package.json"), installed.join("package.json"))
        .expect("the package manifest is copyable");
    copy_tree(&package.join("src"), &installed.join("src"));

    root
}

fn run(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(ANUBIS)
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the anubis binary runs")
}

fn copy_tree(from: &Path, to: &Path) {
    create(to);
    for entry in std::fs::read_dir(from).expect("the package's source is readable") {
        let entry = entry.expect("package entries are readable");
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).expect("package files are copyable");
        }
    }
}

fn create(directory: &Path) {
    std::fs::create_dir_all(directory).expect("scratch directories are creatable");
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn remove(root: &Path) {
    std::fs::remove_dir_all(root).expect("scratch trees are removable");
}
