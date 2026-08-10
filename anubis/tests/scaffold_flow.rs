//! `anubis scaffold model` against a real copy of the starter application.
//!
//! The scaffolder's inputs are files the application owns, so the only honest
//! test is to run the real binary in a real application tree: the starter is
//! copied to a scratch directory, two models are scaffolded into it (one owned
//! by a team, one owned through the first), and the result is inspected as
//! text. Compiling the copy is left to the workspace's own `cargo test`, which
//! builds the starter after a scaffold during development.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANUBIS: &str = env!("CARGO_BIN_EXE_anubis");

/// Directories that are build output rather than application source.
const SKIPPED: [&str; 5] = ["target", "node_modules", "dist", ".vite", "coverage"];

/// Names the living templates use that must never survive a transformation.
const TEMPLATE_TOKENS: [&str; 7] = [
    "CreativeConcept",
    "creative_concept",
    "creative-concept",
    "TangibleThing",
    "tangible_thing",
    "absolutely_abstract",
    "completely_concrete",
];

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scaffold run inspected artifact by artifact"
)]
fn scaffolding_two_models_writes_a_full_backend_slice() {
    let app = copy_starter("full-slice");

    let output = scaffold(&app, &["Project", "Team", "name:text_field"]);
    assert!(
        output.status.success(),
        "scaffolding Project failed: {}",
        stderr(&output),
    );

    let output = scaffold(&app, &["Goal", "Project,Team", "name:text_field"]);
    assert!(
        output.status.success(),
        "scaffolding Goal failed: {}",
        stderr(&output),
    );

    // Every artifact one run owes the application.
    for expected in [
        "backend/src/projects/mod.rs",
        "backend/src/projects/model.rs",
        "backend/src/projects/routes.rs",
        "backend/src/goals/mod.rs",
        "backend/src/goals/model.rs",
        "backend/src/goals/routes.rs",
        "backend/tests/projects_flow.rs",
        "backend/tests/goals_flow.rs",
    ] {
        assert!(app.join(expected).is_file(), "missing {expected}");
    }
    let project_migration = migration(&app, "_create_projects");
    assert!(project_migration.join("up.sql").is_file());
    assert!(project_migration.join("down.sql").is_file());
    let goal_migration = migration(&app, "_create_goals");
    assert!(goal_migration.join("up.sql").is_file());

    // The migration carries the table, its index, and the shared trigger.
    let up = read(&project_migration.join("up.sql"));
    assert!(up.contains("CREATE TABLE projects ("), "up.sql: {up}");
    assert!(up.contains("team_id UUID NOT NULL REFERENCES teams(id)"));
    assert!(up.contains("CREATE TRIGGER set_updated_at BEFORE UPDATE ON projects"));
    assert_eq!(
        read(&project_migration.join("down.sql")).trim(),
        "DROP TABLE projects;"
    );
    assert!(read(&goal_migration.join("up.sql")).contains("REFERENCES projects(id)"));

    // The nested model reaches its parent through the module the earlier
    // scaffold created, not through the template it came from.
    let goal_model = read(&app.join("backend/src/goals/model.rs"));
    assert!(
        goal_model.contains("use crate::projects::Project;"),
        "{goal_model}"
    );
    assert!(goal_model.contains("pub async fn valid_projects("));
    assert!(goal_model.contains("pub project_id: Uuid,"));

    // No template name survives anywhere in the generated files.
    for generated in [
        "backend/src/projects/model.rs",
        "backend/src/projects/routes.rs",
        "backend/src/goals/model.rs",
        "backend/src/goals/routes.rs",
        "backend/tests/projects_flow.rs",
        "backend/tests/goals_flow.rs",
    ] {
        let contents = read(&app.join(generated));
        for token in TEMPLATE_TOKENS {
            assert!(
                !contents.contains(token),
                "{generated} still contains `{token}`",
            );
        }
    }

    // The shared files received their insertions, above the anchors, once.
    let schema = read(&app.join("backend/src/schema.rs"));
    assert!(schema.contains("    projects (id) {"), "{schema}");
    assert!(schema.contains("    goals (id) {"));
    assert!(schema.contains("diesel::joinable!(goals -> projects (project_id));"));
    assert!(schema.contains("diesel::allow_tables_to_appear_in_same_query!(projects, goals);"));
    assert!(
        !schema.contains("diesel::joinable!(projects ->"),
        "a team-owned model joins nothing: {schema}",
    );
    assert_anchored(&schema, "🐺 anubis:tables", "projects (id) {");
    assert_anchored(&schema, "🐺 anubis:joins", "diesel::joinable!(goals");

    let library = read(&app.join("backend/src/lib.rs"));
    assert!(library.contains("pub mod projects;"), "{library}");
    assert!(library.contains("pub mod goals;"));
    assert!(
        library.contains("router = router.merge(projects::router(pool.clone(), roles.clone()));")
    );
    assert_anchored(&library, "🐺 anubis:modules", "pub mod projects;");
    assert_anchored(&library, "🐺 anubis:routes", "merge(goals::router");

    let roles = read(&app.join("config/roles.yml"));
    assert_eq!(roles.matches("Project: [read]").count(), 1, "{roles}");
    assert_eq!(roles.matches("Project: [manage]").count(), 1);
    assert_eq!(roles.matches("Goal: [read]").count(), 1);
    assert_anchored(&roles, "🐺 anubis:models:default", "Project: [read]");
    assert_anchored(&roles, "🐺 anubis:models:editor", "Goal: [manage]");

    // The frontend's permissions module is regenerated in the same run.
    let generated_roles = read(&app.join("frontend/src/roles.generated.ts"));
    assert!(generated_roles.contains("Project"), "{generated_roles}");
    assert!(generated_roles.contains("Goal"));

    // A second run refuses cleanly, leaving the first run's work alone.
    let rerun = scaffold(&app, &["Project", "Team", "name:text_field"]);
    assert!(!rerun.status.success(), "a repeat scaffold must refuse");
    assert!(
        stderr(&rerun).contains("backend/src/projects already exists"),
        "unexpected error: {}",
        stderr(&rerun),
    );
    assert_eq!(
        read(&app.join("config/roles.yml"))
            .matches("Project: [read]")
            .count(),
        1,
        "a refused run must not touch the shared files",
    );

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// The same command serves an application stamped by `anubis new`, which is
/// the only application most developers ever scaffold into.
#[test]
fn a_stamped_application_scaffolds_the_same_way() {
    let scratch = std::env::temp_dir().join(format!("anubis-scaffold-new-{}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).expect("scratch directories are removable");
    }
    std::fs::create_dir_all(&scratch).expect("scratch directories are creatable");

    let stamped = Command::new(ANUBIS)
        .args(["new", "acme-crm"])
        .current_dir(&scratch)
        .output()
        .expect("the anubis binary runs");
    assert!(
        stamped.status.success(),
        "anubis new failed: {}",
        stderr(&stamped)
    );

    let app = scratch.join("acme-crm");
    // From a subdirectory, so the app-root search has to walk up.
    let output = Command::new(ANUBIS)
        .args(["scaffold", "model", "Invoice", "Team", "name:text_field"])
        .current_dir(app.join("backend/src"))
        .output()
        .expect("the anubis binary runs");
    assert!(
        output.status.success(),
        "scaffolding into a stamped app failed: {}",
        stderr(&output),
    );

    assert!(app.join("backend/src/invoices/model.rs").is_file());
    let test = read(&app.join("backend/tests/invoices_flow.rs"));
    assert!(
        test.contains("the_invoice_slice_serves_full_crud"),
        "the stamped test keeps its narrative: {test}",
    );
    let support = read(&app.join("backend/tests/support/mod.rs"));
    assert!(
        support.contains("acme_crm::APP_MIGRATIONS"),
        "the stamped app keeps its own crate name: {support}",
    );

    std::fs::remove_dir_all(&scratch).expect("scratch directories are removable");
}

#[test]
fn arguments_and_locations_are_rejected_with_a_reason() {
    let app = copy_starter("rejections");

    let deep = scaffold(&app, &["Task", "Goal,Project,Team"]);
    assert!(!deep.status.success());
    assert!(
        stderr(&deep).contains("roadmap"),
        "deeper chains must point at the roadmap: {}",
        stderr(&deep),
    );

    let unowned = scaffold(&app, &["Task", "Project"]);
    assert!(!unowned.status.success());
    assert!(stderr(&unowned).contains("must end in `Team`"));

    let unsupported = scaffold(&app, &["Task", "Team", "due:date_field"]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("text_field, text_area"),
        "the supported types must be listed: {}",
        stderr(&unsupported),
    );

    // Nothing was written by any refused run.
    assert!(!app.join("backend/src/tasks").exists());

    // Outside an application, the command says so and names what it looks for.
    let outside = Command::new(ANUBIS)
        .args(["scaffold", "model", "Task", "Team"])
        .current_dir(app.parent().expect("the scratch root exists"))
        .output()
        .expect("the anubis binary runs");
    assert!(!outside.status.success());
    assert!(
        stderr(&outside).contains("config/roles.yml"),
        "unexpected error: {}",
        stderr(&outside),
    );

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// Runs `anubis scaffold model` inside `app`.
fn scaffold(app: &Path, arguments: &[&str]) -> Output {
    Command::new(ANUBIS)
        .args(["scaffold", "model"])
        .args(arguments)
        .current_dir(app)
        .output()
        .expect("the anubis binary runs")
}

/// Copies the repository's starter application into a scratch directory.
fn copy_starter(label: &str) -> PathBuf {
    let starter = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../starter"));
    let scratch = std::env::temp_dir()
        .join(format!("anubis-scaffold-{label}-{}", std::process::id()))
        .join("app");
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).expect("scratch directories are removable");
    }
    copy_tree(&starter, &scratch);
    scratch
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("scratch directories are creatable");
    for entry in std::fs::read_dir(source).expect("the starter tree is readable") {
        let entry = entry.expect("starter entries are readable");
        let name = entry.file_name();
        let name = name.to_str().expect("starter paths are UTF-8");
        let path = entry.path();
        if path.is_dir() {
            if !SKIPPED.contains(&name) {
                copy_tree(&path, &destination.join(name));
            }
        } else {
            std::fs::copy(&path, destination.join(name)).expect("starter files are copyable");
        }
    }
}

/// The migration directory whose name ends in `suffix`.
fn migration(app: &Path, suffix: &str) -> PathBuf {
    let migrations = app.join("backend/migrations");
    std::fs::read_dir(&migrations)
        .expect("the migrations directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(suffix))
        })
        .unwrap_or_else(|| {
            panic!(
                "no migration ending in {suffix} under {}",
                migrations.display()
            )
        })
}

/// Asserts `addition` sits above `anchor`, which is what keeps a file
/// re-scaffoldable.
fn assert_anchored(contents: &str, anchor: &str, addition: &str) {
    let addition_at = contents
        .find(addition)
        .unwrap_or_else(|| panic!("`{addition}` is missing:\n{contents}"));
    let anchor_at = contents
        .find(anchor)
        .unwrap_or_else(|| panic!("`{anchor}` is missing:\n{contents}"));
    assert!(
        addition_at < anchor_at,
        "`{addition}` must sit above `{anchor}`:\n{contents}",
    );
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
