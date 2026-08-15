//! `anubis eject`: take ownership of a framework frontend component.
//!
//! The library's [`anubis::eject`] module holds the catalog and the two text
//! transforms; this module is its filesystem shell. It resolves the frontend
//! package the way the application's own bundler does, walking up from the
//! application root through `node_modules`, copies the component's source (and
//! any package-internal file it reads that the package does not export) into
//! `frontend/src/anubis/`, and moves every import of it in the application
//! from the package to the copy.
//!
//! The whole run is planned before anything is written, so a component that is
//! already ejected, a package that is not installed, or a file the package no
//! longer ships stops the command with the application untouched.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anubis::eject::{
    CATALOG, Component, DESTINATION, PACKAGE, banner, eject_module, relative_imports,
    rewrite_import, root_exports,
};

use super::{app_root, display, fail, read};

/// Runs `anubis eject <Component>`, or `anubis eject --list`.
pub(crate) fn run(component: Option<&str>, list: bool) -> ExitCode {
    if list {
        print_catalog();
        return ExitCode::SUCCESS;
    }

    let Some(key) = component else {
        return fail("name a component to eject, or run `anubis eject --list` to see them all");
    };
    let Some(component) = anubis::eject::find(key) else {
        return fail(&unknown_component(key));
    };

    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };
    let package = match package_root(&root) {
        Ok(package) => package,
        Err(reason) => return fail(&reason),
    };
    let version = match package_version(&package) {
        Ok(version) => version,
        Err(reason) => return fail(&reason),
    };

    let plan = match plan(&root, &package, component, &version) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };
    if let Err(reason) = plan.apply(&root) {
        return fail(&reason);
    }

    report(component, &version, &plan);
    ExitCode::SUCCESS
}

/// Everything one ejection writes, computed before anything touches disk.
struct Plan {
    /// New files, as `(path relative to the app root, contents)`.
    created: Vec<(PathBuf, String)>,
    /// Package-internal files an earlier ejection already brought over.
    reused: Vec<PathBuf>,
    /// Application files whose import of the component moves to the copy.
    rewired: Vec<(PathBuf, String)>,
}

impl Plan {
    fn apply(&self, root: &Path) -> Result<(), String> {
        for (relative, contents) in self.created.iter().chain(&self.rewired) {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            std::fs::write(&path, contents)
                .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        }
        Ok(())
    }
}

/// Plans the copies and the rewiring, reading the installed package.
fn plan(root: &Path, package: &Path, component: Component, version: &str) -> Result<Plan, String> {
    let destination = destination_of(component.source)?;
    if root.join(&destination).is_file() {
        return Err(format!(
            "{} is already ejected: it lives in {}. Edit it there, or delete it to go back to \
             the package's copy.",
            component.key,
            display(&destination),
        ));
    }

    let exports = root_exports(&read(&package.join("src/index.ts"))?);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let mut created = Vec::new();
    let mut reused = Vec::new();
    let mut copied = BTreeSet::new();
    let mut pending = vec![component.source.to_owned()];

    while let Some(source) = pending.pop() {
        if !copied.insert(source.clone()) {
            continue;
        }
        let contents = read(&package.join(&source))?;

        // Each of the file's own relative imports takes one of three roads:
        // a dependency the application already owns is reached where it lies,
        // one the package exports is read from the package, and one the
        // package keeps to itself rides along into the application.
        let mut from_package = Vec::new();
        for import in relative_imports(&contents) {
            let target = resolve(package, &source, &import.specifier)?;
            let local = destination_of(&target)?;
            if root.join(&local).is_file() {
                reused.push(local);
            } else if import.is_public(&exports) {
                from_package.push(import.specifier);
            } else {
                pending.push(target);
            }
        }

        let note = banner(&source, version, &today);
        created.push((
            destination_of(&source)?,
            eject_module(&contents, &note, &from_package),
        ));
    }

    // Copy order follows the dependency walk; the report does not.
    created.sort_by(|left, right| left.0.cmp(&right.0));
    reused.sort();
    reused.dedup();

    let rewired = rewire(root, component.key, &destination)?;
    Ok(Plan {
        created,
        reused,
        rewired,
    })
}

/// Moves every application import of `key` from the package to the copy.
fn rewire(root: &Path, key: &str, destination: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let destination = display(destination);
    let mut rewired = Vec::new();

    for relative in typescript_files(root)? {
        let contents = read(&root.join(&relative))?;
        let consumer = display(&relative);
        let specifier = local_specifier(&consumer, &destination);
        if let Some(updated) = rewrite_import(&contents, key, &specifier) {
            rewired.push((relative, updated));
        }
    }

    Ok(rewired)
}

/// Every TypeScript file under `frontend/src`, outside the ejected tree.
///
/// The ejected tree is skipped because its files already reach each other by
/// the relative paths the package wrote; only the application's own modules
/// import the framework by name.
fn typescript_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect(root, &PathBuf::from("frontend/src"), &mut files)?;
    files.sort();
    Ok(files)
}

fn collect(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if relative == Path::new(DESTINATION) {
        return Ok(());
    }

    let directory = root.join(relative);
    let entries = std::fs::read_dir(&directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
        let path = relative.join(entry.file_name());
        if entry.path().is_dir() {
            collect(root, &path, files)?;
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("ts" | "tsx"),
        ) {
            files.push(path);
        }
    }
    Ok(())
}

/// The path a package file lands at, relative to the application root.
fn destination_of(source: &str) -> Result<PathBuf, String> {
    let relative = source.strip_prefix("src/").ok_or_else(|| {
        format!("{source} lives outside the package's src/, so it has no home in the application")
    })?;
    Ok(PathBuf::from(DESTINATION).join(relative))
}

/// The package file a relative import in `source` reads, as a package path.
fn resolve(package: &Path, source: &str, specifier: &str) -> Result<String, String> {
    let directory = source.rsplit_once('/').map_or("", |(head, _file)| head);
    let mut parts = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for segment in specifier.split('/') {
        match segment {
            "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    let path = parts.join("/");

    // TypeScript imports carry no extension, so the module is whichever of the
    // two the package actually ships.
    for extension in [".tsx", ".ts"] {
        let candidate = format!("{path}{extension}");
        if package.join(&candidate).is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{source} imports `{specifier}`, which is neither {path}.tsx nor {path}.ts in {}",
        package.display(),
    ))
}

/// The specifier `consumer` reaches `destination` by, both app-relative.
fn local_specifier(consumer: &str, destination: &str) -> String {
    let directory = consumer.rsplit_once('/').map_or("", |(head, _file)| head);
    let module = destination
        .strip_suffix(".tsx")
        .or_else(|| destination.strip_suffix(".ts"))
        .unwrap_or(destination);

    let mut from = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut to = module.split('/').collect::<Vec<_>>();
    // The last segment is the module's own name, so it never joins the shared
    // prefix: a file in the same directory is `./Name`, not `Name`.
    while !from.is_empty() && to.len() > 1 && from[0] == to[0] {
        from.remove(0);
        to.remove(0);
    }

    let prefix = if from.is_empty() {
        "./".to_owned()
    } else {
        "../".repeat(from.len())
    };
    format!("{prefix}{}", to.join("/"))
}

/// The installed frontend package, resolved the way the bundler resolves it.
///
/// The search starts at the application's frontend and walks up, so it finds
/// both arrangements: yarn hoists a workspace dependency to the workspace root
/// (which in this repository is the framework's own `frontend/`, symlinked),
/// while a standalone install keeps it beside the package that asked for it.
fn package_root(root: &Path) -> Result<PathBuf, String> {
    for ancestor in root.join("frontend").ancestors() {
        let candidate = ancestor.join("node_modules").join(PACKAGE);
        if candidate.join("package.json").is_file() {
            return Ok(candidate);
        }
    }

    Err(format!(
        "{PACKAGE} is not installed under {}: run `yarn install` first. Ejecting copies the \
         component out of the installed package, so the copy matches the version this \
         application runs.",
        root.display(),
    ))
}

/// The version of the installed package, for the ejected file's provenance.
fn package_version(package: &Path) -> Result<String, String> {
    let manifest = package.join("package.json");
    let parsed = serde_json::from_str::<serde_json::Value>(&read(&manifest)?)
        .map_err(|error| format!("{} is not valid JSON: {error}", manifest.display()))?;

    parsed["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} declares no version", manifest.display()))
}

/// The message an unknown component earns, with the catalog spelled out.
fn unknown_component(key: &str) -> String {
    let known = CATALOG
        .iter()
        .map(|component| component.key)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "`{key}` is not an ejectable component. Anubis ejects the field component library: \
         {known}. Everything else the package ships stays framework-owned, because a copy of it \
         would fork the protocol it speaks with the backend rather than restyle a control.",
    )
}

/// Prints every component an application may take ownership of.
fn print_catalog() {
    let width = CATALOG
        .iter()
        .map(|component| component.key.len())
        .max()
        .unwrap_or_default();

    println!("The frontend package ships these files for an application to own:");
    println!();
    for component in CATALOG {
        println!(
            "  {:width$}  {}",
            component.key,
            component.summary,
            width = width,
        );
    }

    println!();
    println!(
        "Eject one with `anubis eject TextField`. It lands in {DESTINATION}/, carries a note \
         saying where it came from, and every import of it in this application moves to the copy."
    );
    println!(
        "From then on the file is yours: upgrading {PACKAGE} no longer improves it. Delete it \
         to go back to the package's copy."
    );
    println!();
    println!(
        "Everything else the package ships stays framework-owned on purpose. The API client, \
         the realtime client, the React hooks, and the WebAuthn helpers speak a protocol the \
         backend keeps moving, so a copy would fork that contract instead of restyling a \
         control; compose them from the pages this application already owns."
    );
}

/// Prints what was copied, what was rewired, and what it now costs.
fn report(component: Component, version: &str, plan: &Plan) {
    println!("ejected {} from {PACKAGE} v{version}", component.key);

    println!();
    println!("created:");
    for (path, _contents) in &plan.created {
        println!("  {}", display(path));
    }
    if !plan.reused.is_empty() {
        println!("already ejected, so left alone:");
        for path in &plan.reused {
            println!("  {}", display(path));
        }
    }

    if plan.rewired.is_empty() {
        println!("rewired: nothing, no file in this application imported it from the package");
    } else {
        println!("rewired:");
        for (path, _contents) in &plan.rewired {
            println!("  {}", display(path));
        }
    }

    if plan.created.len() > 1 {
        println!();
        println!(
            "The extra files are package-internal: {} reads them and the package does not \
             export them, so they came along rather than being left behind an import that \
             would not resolve.",
            component.key,
        );
    }

    println!();
    println!(
        "{} is yours now: upgrading {PACKAGE} no longer improves it, and a fix that lands \
         upstream has to be brought over by hand. Delete the file to go back to the package's \
         copy.",
        component.key,
    );

    println!();
    println!("Next steps:");
    println!("  yarn typecheck");
    println!("  yarn lint");
}

#[cfg(test)]
mod tests {
    use super::{destination_of, local_specifier, unknown_component};

    #[test]
    fn a_package_file_lands_under_the_mirrored_tree() {
        assert_eq!(
            destination_of("src/fields/internal/TextualField.tsx").unwrap(),
            std::path::PathBuf::from("frontend/src/anubis/fields/internal/TextualField.tsx"),
        );
        destination_of("dist/index.js").expect_err("only the package's source is ejectable");
    }

    #[test]
    fn a_consumer_reaches_the_copy_by_a_relative_path() {
        let destination = "frontend/src/anubis/fields/TextField.tsx";
        assert_eq!(
            local_specifier(
                "frontend/src/components/settings/PasskeysCard.tsx",
                destination
            ),
            "../../anubis/fields/TextField",
        );
        assert_eq!(
            local_specifier("frontend/src/App.tsx", destination),
            "./anubis/fields/TextField",
        );
        assert_eq!(
            local_specifier("frontend/src/anubis/fields/Other.tsx", destination),
            "./TextField",
        );
    }

    #[test]
    fn an_unknown_component_is_refused_with_the_catalog() {
        let message = unknown_component("AnubisProvider");
        assert!(
            message.contains("`AnubisProvider` is not an ejectable component"),
            "{message}"
        );
        assert!(message.contains("TextField"), "{message}");
    }
}
