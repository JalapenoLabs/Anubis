//! `anubis upgrade`: move an application to another framework release.
//!
//! The library's [`anubis::upgrade`] module holds the manifest surgery; this
//! module is its shell. It finds the application root, reads the two
//! manifests, resolves the version to move to (from `--to`, or from
//! crates.io), rewrites both requirements, realizes the bump in the two
//! lockfiles, and re-runs every generator the application carries so the
//! generated files match the framework that will compile them.
//!
//! The whole run is planned before anything is written, and `--dry-run` stops
//! after printing that plan. The registry is only ever reached when `--to` is
//! absent, which is the seam the tests use: a target named on the command line
//! makes the whole command offline.
//!
//! Upgrading starts at published versions. An application that tracks the
//! framework from git, from a path, or through yarn's workspace protocol has
//! no version to bump, and the command says so rather than inventing one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use anubis::upgrade::{self, Declaration, Dependency, Latest};
use semver::Version;

use super::{app_root, display, fail, read};

/// The manifest declaring the framework crate, as `anubis`.
const BACKEND_MANIFEST: &str = "backend/Cargo.toml";

/// The manifest declaring the `@jalapenolabs/anubis` package.
const FRONTEND_MANIFEST: &str = "frontend/package.json";

/// The sources the generators compile, and the files they compile them into.
/// These are the three files the application's CI checks for drift.
const ROLES_YML: &str = "config/roles.yml";
const ROLES_TS: &str = "frontend/src/roles.generated.ts";
const BILLING_YML: &str = "config/billing.yml";
const PLANS_TS: &str = "frontend/src/plans.generated.ts";
const CLIENT_TS: &str = "frontend/src/api/v1.generated.ts";

/// How long crates.io gets to answer which version is the latest.
///
/// Generous for one small JSON read, short enough that an unreachable registry
/// does not look like a hung command.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `anubis upgrade [--to <version>] [--dry-run]`.
pub(crate) fn run(to: Option<&str>, dry_run: bool) -> ExitCode {
    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };

    let backend = match read(&root.join(BACKEND_MANIFEST)) {
        Ok(backend) => backend,
        Err(reason) => return fail(&reason),
    };
    let frontend = match read(&root.join(FRONTEND_MANIFEST)) {
        Ok(frontend) => frontend,
        Err(reason) => return fail(&reason),
    };

    let cargo = match upgrade::cargo_declaration(&backend) {
        Ok(cargo) => cargo,
        Err(error) => return fail(error.message()),
    };
    let package = match upgrade::package_declaration(&frontend) {
        Ok(package) => package,
        Err(error) => return fail(error.message()),
    };

    // Only a published version can be bumped. Everything else is a real
    // arrangement with nothing to move, so it is named rather than guessed at.
    if cargo.version().is_none() || package.version().is_none() {
        return fail(&unversioned(&cargo, &package));
    }

    let target = match to {
        Some(text) => match Version::parse(text) {
            Ok(target) => target,
            Err(error) => {
                return fail(&format!(
                    "`{text}` is not a release version ({error}); name one as `0.3.0`",
                ));
            }
        },
        None => match latest_published() {
            Ok(Latest::Published(target)) => target,
            Ok(Latest::Absent) => return fail(&not_published(None)),
            Ok(Latest::Foreign { repository }) => {
                return fail(&not_published(repository.as_deref()));
            }
            Err(reason) => return fail(&reason),
        },
    };

    let name = match upgrade::crate_name(&backend) {
        Ok(name) => name.to_owned(),
        Err(error) => return fail(error.message()),
    };

    let plan = match plan(&root, &cargo, &package, &name, target) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };

    if plan.rewrites.is_empty() {
        println!("{name} is already on anubis {}", plan.target);
        println!("Release notes: {}", upgrade::release_notes(&plan.target));
        return ExitCode::SUCCESS;
    }

    println!("upgrade {name} to anubis {}", plan.target);
    println!();

    if dry_run {
        plan.print();
        return ExitCode::SUCCESS;
    }
    match plan.apply(&root) {
        Ok(()) => {
            println!();
            println!("Release notes: {}", upgrade::release_notes(&plan.target));
            println!(
                "Read them before deploying: pre-1.0 a minor release may break, and the notes \
                 are where a break and the steps it takes by hand are named."
            );
            ExitCode::SUCCESS
        }
        Err(reason) => fail(&reason),
    }
}

/// Everything one upgrade does, computed before anything touches disk.
struct Plan {
    target: Version,
    rewrites: Vec<Rewrite>,
    steps: Vec<Step>,
}

/// One manifest's requirement, moved from one version to another.
struct Rewrite {
    /// The manifest, relative to the application root.
    path: PathBuf,
    /// The requirement as the manifest writes it today.
    from: String,
    /// The requirement it is replaced with.
    to: String,
    /// The whole manifest, rewritten.
    contents: String,
}

impl Plan {
    /// Prints what the run would do, for `--dry-run`.
    fn print(&self) {
        let width = self
            .rewrites
            .iter()
            .map(|rewrite| display(&rewrite.path).len())
            .max()
            .unwrap_or_default();

        println!("rewrite:");
        for rewrite in &self.rewrites {
            println!(
                "  {:width$}  {} -> {}",
                display(&rewrite.path),
                rewrite.from,
                rewrite.to,
                width = width,
            );
        }
        println!("then run:");
        for step in &self.steps {
            for line in step.commands() {
                println!("  {line}");
            }
        }

        println!();
        println!("--dry-run: nothing was written.");
        println!("Release notes: {}", upgrade::release_notes(&self.target));
    }

    /// Writes the manifests, then performs every step in order.
    ///
    /// A step that fails stops the run with its own output already on the
    /// terminal. The manifests are written first and stay written: the bump is
    /// the part worth keeping, and the steps after it are the ones a developer
    /// re-runs by hand.
    fn apply(&self, root: &Path) -> Result<(), String> {
        for rewrite in &self.rewrites {
            let path = root.join(&rewrite.path);
            std::fs::write(&path, &rewrite.contents)
                .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
            println!(
                "rewrote {}  {} -> {}",
                display(&rewrite.path),
                rewrite.from,
                rewrite.to,
            );
        }

        let mut generated = Vec::new();
        for step in &self.steps {
            println!();
            for line in step.commands() {
                println!("$ {line}");
            }
            generated.extend(step.perform(root)?);
        }

        if !generated.is_empty() {
            println!();
            println!("regenerated:");
            for path in &generated {
                println!("  {}", display(path));
            }
        }
        Ok(())
    }
}

/// One thing the upgrade does after the manifests are rewritten.
///
/// The two realize steps shell out, because resolving a lockfile is cargo's
/// and yarn's own work. The three generators run in this process against the
/// same library functions their subcommands call, so the version of the
/// framework that regenerates is the version of the CLI being run.
enum Step {
    /// Resolves the new crate version into `Cargo.lock`.
    CargoUpdate,
    /// Resolves the new package version into `yarn.lock`.
    YarnInstall,
    /// The frontend permissions module, compiled from `config/roles.yml`.
    Roles,
    /// The frontend plan catalog, compiled from `config/billing.yml`.
    Plans,
    /// The API client: the application exports its own OpenAPI document, and
    /// the framework renders that export.
    Client { package: String },
}

impl Step {
    /// The command lines a developer would type to do this step by hand.
    fn commands(&self) -> Vec<String> {
        match self {
            Self::CargoUpdate => vec![format!("cargo update -p {}", upgrade::CRATE)],
            Self::YarnInstall => vec!["yarn install".to_owned()],
            Self::Roles => vec![format!(
                "anubis roles generate-ts --file {ROLES_YML} --out {ROLES_TS}",
            )],
            Self::Plans => vec![format!(
                "anubis billing generate-ts --file {BILLING_YML} --out {PLANS_TS}",
            )],
            Self::Client { package } => vec![
                format!("cargo run --quiet -p {package} -- openapi > openapi.json"),
                format!("anubis client generate-ts --from openapi.json --out {CLIENT_TS}"),
            ],
        }
    }

    /// Performs the step, answering with the files it generated.
    fn perform(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        match self {
            Self::CargoUpdate => {
                execute(root, "cargo", &["update", "-p", upgrade::CRATE])?;
                Ok(Vec::new())
            }
            Self::YarnInstall => {
                execute(root, "yarn", &["install"])?;
                Ok(Vec::new())
            }
            Self::Roles => {
                let yaml = read(&root.join(ROLES_YML))?;
                let set = anubis::roles::RoleSet::from_yaml(&yaml)
                    .map_err(|error| format!("{ROLES_YML} is invalid: {error}"))?;
                generate(root, ROLES_TS, &set.to_typescript())
            }
            Self::Plans => {
                let yaml = read(&root.join(BILLING_YML))?;
                let plans = anubis::billing::PlanSet::from_yaml(&yaml)
                    .map_err(|error| format!("{BILLING_YML} is invalid: {error}"))?;
                generate(root, PLANS_TS, &plans.to_typescript())
            }
            Self::Client { package } => {
                let exported = export_openapi(root, package)?;
                let document = serde_json::from_str(&exported).map_err(|error| {
                    format!("{package} exported something that is not an OpenAPI document: {error}")
                })?;
                generate(
                    root,
                    CLIENT_TS,
                    &anubis::api::v1::typescript_client_from(&document),
                )
            }
        }
    }
}

/// Plans the rewrites and the steps, reading what the application carries.
fn plan(
    root: &Path,
    cargo: &Declaration<'_>,
    package: &Declaration<'_>,
    name: &str,
    target: Version,
) -> Result<Plan, String> {
    let mut rewrites = Vec::new();
    for (path, declaration) in [(BACKEND_MANIFEST, cargo), (FRONTEND_MANIFEST, package)] {
        // A requirement already naming the target is left alone, so a rerun
        // after a half-finished upgrade rewrites only what is still behind.
        if declaration.version() == Some(&target) {
            continue;
        }
        let contents = declaration
            .upgraded(&target)
            .map_err(|error| format!("{path}: {}", error.message()))?;
        rewrites.push(Rewrite {
            path: PathBuf::from(path),
            from: declaration.requirement().to_owned(),
            to: requirement(declaration, &target),
            contents,
        });
    }

    let mut steps = vec![Step::CargoUpdate, Step::YarnInstall, Step::Roles];
    // An application that sells nothing has no plans file, and needs none.
    if root.join(BILLING_YML).is_file() {
        steps.push(Step::Plans);
    }
    steps.push(Step::Client {
        package: name.to_owned(),
    });

    Ok(Plan {
        target,
        rewrites,
        steps,
    })
}

/// The requirement `declaration` carries once it names `target`.
fn requirement(declaration: &Declaration<'_>, target: &Version) -> String {
    format!("{}{target}", declaration.operator())
}

/// Writes one generated file, answering with its path for the report.
fn generate(root: &Path, relative: &str, contents: &str) -> Result<Vec<PathBuf>, String> {
    let path = root.join(relative);
    std::fs::write(&path, contents)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(vec![PathBuf::from(relative)])
}

/// The OpenAPI document the application's own binary exports.
///
/// The document belongs to the application (it merges the framework's half
/// with one line per scaffolded model), so the application is what exports it.
/// Its stderr is inherited, which is what puts cargo's compile progress, and
/// any compile error the new framework version causes, on the terminal.
fn export_openapi(root: &Path, package: &str) -> Result<String, String> {
    let output = command("cargo")
        .args(["run", "--quiet", "-p", package, "--", "openapi"])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("failed to run cargo: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "`cargo run -p {package} -- openapi` failed; its output is above. The application \
             has to compile against anubis before the client can be generated from it.",
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| {
        format!("{package} exported an OpenAPI document that is not UTF-8: {error}")
    })
}

/// Runs `program` in the application root, with its output on the terminal.
fn execute(root: &Path, program: &str, arguments: &[&str]) -> Result<(), String> {
    let status = command(program)
        .args(arguments)
        .current_dir(root)
        .status()
        .map_err(|error| format!("failed to run `{program}`: {error}"))?;

    if status.success() {
        return Ok(());
    }
    Err(format!(
        "`{program} {}` failed; its output is above",
        arguments.join(" "),
    ))
}

/// A command builder that finds `.cmd` shims on Windows.
///
/// Yarn is a shim there, exactly as it is for `anubis doctor`, so both go
/// through the shell that a terminal would resolve them with.
fn command(program: &str) -> Command {
    if cfg!(windows) {
        let mut shell = Command::new("cmd");
        shell.args(["/C", program]);
        shell
    } else {
        Command::new(program)
    }
}

/// Asks crates.io what the latest published version is.
fn latest_published() -> Result<Latest, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start an async runtime: {error}"))?;

    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REGISTRY_TIMEOUT)
            // crates.io refuses a request that does not say who is asking.
            .user_agent(format!(
                "anubis/{} (https://github.com/JalapenoLabs/Anubis)",
                anubis::VERSION,
            ))
            .build()
            .map_err(|error| format!("failed to build an HTTP client: {error}"))?;

        let response = client
            .get(upgrade::REGISTRY_ENDPOINT)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                format!(
                    "failed to reach crates.io: {error}. Name a version with `--to` to upgrade \
                     without the registry.",
                )
            })?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Latest::Absent);
        }
        if !response.status().is_success() {
            return Err(format!("crates.io answered {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|error| format!("failed to read the crates.io response: {error}"))?;
        upgrade::latest_published(&body).map_err(|error| error.message().to_owned())
    })
}

/// What the registry not carrying the framework means, and what to do.
///
/// `held_by` is the repository of the crate holding the name, when one holds
/// it. Naming it is the difference between "not released yet" and "released
/// under a name somebody else owns", and only one of those is a surprise.
fn not_published(held_by: Option<&str>) -> String {
    let crate_name = upgrade::CRATE;
    let mut message = match held_by {
        None => format!(
            "crates.io carries no crate named `{crate_name}`, so there is no published version to \
             upgrade to.",
        ),
        Some(repository) => format!(
            "crates.io carries a crate named `{crate_name}`, but it is not this framework: it \
             comes from {repository}. The framework has not been published, and this command will \
             not move an application onto somebody else's crate.",
        ),
    };
    message.push_str(
        " Until the first release the framework is tracked from its git repository: `cargo \
         update` and `yarn up @jalapenolabs/anubis` move an application to its latest commit. \
         Name a version with `--to` once one is published. See docs/upgrading.md.",
    );
    message
}

/// The message an application with nothing to bump earns.
fn unversioned(cargo: &Declaration<'_>, package: &Declaration<'_>) -> String {
    let mut message = String::from(
        "this application does not resolve the framework from a published version, so there is \
         nothing for `anubis upgrade` to bump:\n",
    );
    for (path, declaration) in [(BACKEND_MANIFEST, cargo), (FRONTEND_MANIFEST, package)] {
        let source = match declaration.dependency() {
            Dependency::Published(version) => format!("anubis {version}"),
            Dependency::Git => "the framework's git repository".to_owned(),
            Dependency::Path => "a local path".to_owned(),
            Dependency::Workspace => "this workspace".to_owned(),
        };
        writeln!(
            message,
            "  {path}: {source} ({})",
            declaration.requirement()
        )
        .expect("writing to a String cannot fail");
    }
    message.push_str(
        "A git-tracked application follows the framework's default branch: `cargo update` and \
         `yarn up @jalapenolabs/anubis` move it to the latest commit. Point both dependencies at \
         a published version to upgrade by release instead. See docs/upgrading.md.",
    );
    message
}

#[cfg(test)]
mod tests {
    use super::{Step, not_published, plan, requirement, unversioned};
    use anubis::upgrade::{cargo_declaration, package_declaration};
    use semver::Version;

    const CARGO: &str = "[package]\nname = \"acme\"\n\n[dependencies]\nanubis = { package = \
                         \"anubis-framework\", version = \"0.2.0\" }\n";
    const PACKAGE: &str = "{ \"dependencies\": { \"@jalapenolabs/anubis\": \"^0.2.0\" } }";

    fn version(text: &str) -> Version {
        text.parse().expect("the test names a version")
    }

    /// A plan names both manifests, and every step the application carries.
    /// The billing generator is absent, because this scratch root holds no
    /// `config/billing.yml`.
    #[test]
    fn a_plan_covers_both_manifests_and_every_generator() {
        let root = std::env::temp_dir().join(format!("anubis-upgrade-plan-{}", std::process::id()));
        let cargo = cargo_declaration(CARGO).expect("the manifest is readable");
        let package = package_declaration(PACKAGE).expect("the manifest is readable");

        let plan = plan(&root, &cargo, &package, "acme", version("0.3.0")).expect("plannable");

        let paths = plan
            .rewrites
            .iter()
            .map(|rewrite| rewrite.path.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        assert_eq!(paths, ["backend/Cargo.toml", "frontend/package.json"]);
        assert_eq!(plan.rewrites[0].to, "0.3.0");
        assert_eq!(plan.rewrites[1].to, "^0.3.0");
        assert!(
            plan.rewrites[0]
                .contents
                .contains("anubis = { package = \"anubis-framework\", version = \"0.3.0\" }"),
        );
        assert!(
            plan.rewrites[1]
                .contents
                .contains("\"@jalapenolabs/anubis\": \"^0.3.0\""),
        );

        let commands = plan
            .steps
            .iter()
            .flat_map(Step::commands)
            .collect::<Vec<_>>()
            .join("\n");
        // The package spec is the published name, not the aliased key.
        assert!(
            commands.contains("cargo update -p anubis-framework"),
            "{commands}"
        );
        assert!(commands.contains("yarn install"), "{commands}");
        assert!(commands.contains("anubis roles generate-ts"), "{commands}");
        assert!(
            !commands.contains("billing generate-ts"),
            "an application with no billing.yml regenerates no plan catalog: {commands}",
        );
        assert!(
            commands.contains("cargo run --quiet -p acme -- openapi"),
            "{commands}",
        );
    }

    /// A manifest already naming the target is left alone, so a rerun after a
    /// half-finished upgrade rewrites only what is behind.
    #[test]
    fn a_manifest_already_on_the_target_is_not_rewritten() {
        let root = std::env::temp_dir();
        let cargo = cargo_declaration(CARGO).expect("the manifest is readable");
        let package = package_declaration(PACKAGE).expect("the manifest is readable");

        let plan = plan(&root, &cargo, &package, "acme", version("0.2.0")).expect("plannable");
        assert!(
            plan.rewrites.is_empty(),
            "both manifests already name 0.2.0",
        );
    }

    #[test]
    fn a_requirement_keeps_its_operator() {
        for (manifest, expected) in [
            ("{ \"@jalapenolabs/anubis\": \"^0.2.0\" }", "^0.3.0"),
            ("{ \"@jalapenolabs/anubis\": \"~0.2.0\" }", "~0.3.0"),
            ("{ \"@jalapenolabs/anubis\": \"0.2.0\" }", "0.3.0"),
        ] {
            let declaration = package_declaration(manifest).expect("the manifest is readable");
            assert_eq!(requirement(&declaration, &version("0.3.0")), expected);
        }
    }

    /// The pre-publish reality: a stamped application tracks git on both ends,
    /// and the refusal says so and names the way forward.
    #[test]
    fn a_git_tracked_application_is_told_what_to_do() {
        let cargo = cargo_declaration(
            "[dependencies]\nanubis = { package = \"anubis-framework\", git = \
             \"https://github.com/JalapenoLabs/Anubis.git\" }\n",
        )
        .expect("the manifest is readable");
        let package = package_declaration(
            "{ \"@jalapenolabs/anubis\": \"https://github.com/JalapenoLabs/Anubis.git\" }",
        )
        .expect("the manifest is readable");

        let message = unversioned(&cargo, &package);
        assert!(message.contains("backend/Cargo.toml"), "{message}");
        assert!(message.contains("frontend/package.json"), "{message}");
        assert!(message.contains("git repository"), "{message}");
        assert!(message.contains("docs/upgrading.md"), "{message}");
    }

    /// The registry's two ways of not carrying the framework read differently,
    /// because a name held by somebody else is the surprising one.
    #[test]
    fn an_unpublished_framework_is_reported_for_the_right_reason() {
        let absent = not_published(None);
        assert!(
            absent.contains("carries no crate named `anubis-framework`"),
            "{absent}"
        );
        assert!(absent.contains("--to"), "{absent}");

        let taken = not_published(Some("https://github.com/qhua948/anubis"));
        assert!(taken.contains("it is not this framework"), "{taken}");
        assert!(taken.contains("qhua948"), "{taken}");
    }
}
