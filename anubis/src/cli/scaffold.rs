//! `anubis scaffold model`: generate a model's backend slice from the
//! application's own living templates.
//!
//! The templates are application code, not framework assets: this command
//! reads `backend/src/scaffolding/`, the migration that created the template's
//! table, and the template's integration test straight out of the application
//! it is run in, rewrites every template name into the target model's names,
//! and writes the result beside them. Shared files (`lib.rs`, `schema.rs`,
//! `config/roles.yml`) receive one line each above their magic anchors, which
//! is idempotent, so re-running a scaffold never duplicates an insertion.
//!
//! Everything is planned before anything is written: a missing template, a
//! missing anchor, or a module that already exists stops the command with the
//! application untouched.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anubis::scaffold::{
    ModelScaffold, ModelTemplate, Replacements, anchor, insert_above_anchor, line_containing,
    table_block,
};

/// The grants a scaffolded model receives, one per role-suffixed anchor.
///
/// `billing` and `admin` include these two roles and so need no entries.
const ROLE_GRANTS: [(&str, &str); 2] = [
    (anchor::ROLES_DEFAULT, "read"),
    (anchor::ROLES_EDITOR, "manage"),
];

/// The Rust edition the framework and every stamped application build on.
const EDITION: &str = "2024";

/// Runs `anubis scaffold model <Model> <ParentChain> [field:type ...]`.
pub(crate) fn model(model: &str, ownership: &str, fields: &[String]) -> ExitCode {
    let scaffold = match ModelScaffold::parse(model, ownership, fields) {
        Ok(scaffold) => scaffold,
        Err(error) => return fail(error.message()),
    };

    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };

    let plan = match plan(&root, &scaffold) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };
    if let Err(reason) = plan.apply(&root) {
        return fail(&reason);
    }

    report(&scaffold, &plan);
    ExitCode::SUCCESS
}

/// Everything one scaffold writes, computed before anything touches disk.
struct Plan {
    /// New files, as `(path relative to the app root, contents)`.
    created: Vec<(PathBuf, String)>,
    /// Application files that gain lines above their anchors.
    updated: Vec<(PathBuf, String)>,
}

impl Plan {
    /// Writes every planned file, then formats the Rust ones.
    fn apply(&self, root: &Path) -> Result<(), String> {
        for (relative, contents) in self.created.iter().chain(&self.updated) {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            std::fs::write(&path, contents)
                .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        }
        format_rust_files(root, self);
        Ok(())
    }

    /// Every path the plan touches.
    fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.created
            .iter()
            .chain(&self.updated)
            .map(|(path, _contents)| path)
    }
}

/// Plans the whole scaffold, reading the application's own templates.
fn plan(root: &Path, scaffold: &ModelScaffold) -> Result<Plan, String> {
    let template = scaffold.template();
    let replacements = scaffold.replacements();

    let mut created = stamp_module(root, scaffold, template, &replacements)?;
    created.push(stamp_test(root, scaffold, template, &replacements)?);
    created.extend(stamp_migration(root, scaffold, template, &replacements)?);

    let schema = update_schema(root, scaffold, template, &replacements)?;
    let library = update_lib(root, scaffold)?;
    let roles = update_roles(root, scaffold)?;
    let roles_client = regenerate_roles_client(root, &roles.1)?;

    Ok(Plan {
        created,
        updated: vec![schema, library, roles, roles_client],
    })
}

/// Stamps the template module into the application's own module directory.
fn stamp_module(
    root: &Path,
    scaffold: &ModelScaffold,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    let source = PathBuf::from("backend/src/scaffolding").join(template.module());
    let destination = PathBuf::from("backend/src").join(scaffold.module());
    if root.join(&destination).exists() {
        return Err(format!(
            "{} already exists; scaffolding never overwrites a model",
            display(&destination),
        ));
    }

    let entries = std::fs::read_dir(root.join(&source))
        .map_err(|error| format!("failed to read {}: {error}", display(&source)))?;

    let note = scaffold.manual_fields_note();
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("failed to read {}: {error}", display(&source)))?
            .path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} has an unreadable name", path.display()))?;

        let mut contents = replacements.apply(&read(&path)?);
        if name == "model.rs"
            && let Some(note) = note.as_deref()
        {
            contents = plant_note(&contents, note).ok_or_else(|| {
                format!("{} imports nothing to plant a note above", display(&source))
            })?;
        }
        files.push((destination.join(name), contents));
    }

    if files.is_empty() {
        return Err(format!("{} holds no template files", display(&source)));
    }
    // Directory order is arbitrary; the report is not.
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

/// Plants the manual-work note above a stamped model's first import.
fn plant_note(contents: &str, note: &str) -> Option<String> {
    let first_import = contents.lines().find(|line| line.starts_with("use "))?;
    // The whole import line is the needle, so nothing else in the file can
    // match it, and the note lands with a blank line under it.
    insert_above_anchor(contents, first_import, &format!("{note}\n")).ok()
}

/// Stamps the template's integration test into the application's test suite.
fn stamp_test(
    root: &Path,
    scaffold: &ModelScaffold,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<(PathBuf, String), String> {
    let tests = PathBuf::from("backend/tests");
    let source = tests.join(format!("{}_flow.rs", template.table()));
    let destination = tests.join(format!("{}_flow.rs", scaffold.table()));
    if root.join(&destination).exists() {
        return Err(format!(
            "{} already exists; move it aside to scaffold this model again",
            display(&destination),
        ));
    }

    let contents = replacements.apply(&read(&root.join(&source))?);
    Ok((destination, contents))
}

/// Stamps the migration that created the template's table.
fn stamp_migration(
    root: &Path,
    scaffold: &ModelScaffold,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    let migrations = root.join("backend/migrations");
    let source = template_migration(&migrations, &template.table())?;
    let version = migration_version(&migrations, chrono::Utc::now())?;
    let destination =
        PathBuf::from("backend/migrations").join(scaffold.migration_directory(&version));

    let mut up = replacements.apply(&read(&source.join("up.sql"))?);
    let extra = scaffold.extra_sql_columns();
    if !extra.is_empty() {
        let block = format!(
            "-- Added by `anubis scaffold model`. Nullable until each column is wired\n\
             -- through the model and its handlers.\n{}\n",
            extra.join("\n"),
        );
        up = insert_above_anchor(&up, "created_at TIMESTAMPTZ", &block)
            .map_err(|error| format!("{}: {error}", source.join("up.sql").display()))?;
    }
    let down = replacements.apply(&read(&source.join("down.sql"))?);

    Ok(vec![
        (destination.join("up.sql"), up),
        (destination.join("down.sql"), down),
    ])
}

/// The migration directory that created `table`.
fn template_migration(migrations: &Path, table: &str) -> Result<PathBuf, String> {
    let suffix = format!("_create_{table}");
    for name in directory_names(migrations)? {
        if name.ends_with(&suffix) {
            return Ok(migrations.join(name));
        }
    }
    Err(format!(
        "no migration named `*{suffix}` in {}; the living template's migration is what a \
         scaffold transforms",
        migrations.display(),
    ))
}

/// A migration version no existing migration already claims.
///
/// Diesel keys a migration by the version prefix of its directory, so two
/// scaffolds run inside the same second would collide. The next free second is
/// close enough to now and keeps the ordering a developer expects.
fn migration_version(
    migrations: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, String> {
    let existing = directory_names(migrations)?;
    let mut candidate = now;
    loop {
        let version = candidate.format("%Y-%m-%d-%H%M%S").to_string();
        let prefix = format!("{version}_");
        if !existing.iter().any(|name| name.starts_with(&prefix)) {
            return Ok(version);
        }
        candidate += chrono::TimeDelta::seconds(1);
    }
}

/// The names of the directories directly under `directory`.
fn directory_names(directory: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Adds the model's table, and its joins, to `backend/src/schema.rs`.
fn update_schema(
    root: &Path,
    scaffold: &ModelScaffold,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("backend/src/schema.rs");
    let schema = read(&root.join(&relative))?;

    let block = table_block(&schema, &template.table()).ok_or_else(|| {
        format!(
            "{} declares no `{}` table; the living template's table is what a scaffold \
             transforms",
            display(&relative),
            template.table(),
        )
    })?;
    let mut block = replacements.apply(&block);
    let extra = scaffold.extra_schema_columns();
    if !extra.is_empty() {
        block = insert_above_anchor(&block, "created_at ->", &format!("{}\n", extra.join("\n")))
            .map_err(|error| format!("{}: {error}", display(&relative)))?;
    }

    let mut updated = insert_above_anchor(&schema, anchor::TABLES, &format!("{block}\n\n"))
        .map_err(|error| format!("{}: {error}", display(&relative)))?;

    // A nested model joins its parent, and the two may share a query. A
    // team-owned model has neither: the framework's `teams` table lives in
    // another crate, which Rust's orphan rules keep out of these macros.
    if scaffold.parent().is_some() {
        for (needle, anchor) in [
            (
                format!("diesel::joinable!({}", template.table()),
                anchor::JOINS,
            ),
            (
                "diesel::allow_tables_to_appear_in_same_query!(".to_owned(),
                anchor::SAME_QUERY,
            ),
        ] {
            let line = line_containing(&schema, &needle).ok_or_else(|| {
                format!(
                    "{} holds no `{needle}` declaration to transform",
                    display(&relative),
                )
            })?;
            updated = insert_above_anchor(&updated, anchor, &replacements.apply(&line))
                .map_err(|error| format!("{}: {error}", display(&relative)))?;
        }
    }

    Ok((relative, updated))
}

/// Declares the model's module and mounts its router in `backend/src/lib.rs`.
fn update_lib(root: &Path, scaffold: &ModelScaffold) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("backend/src/lib.rs");
    let source = read(&root.join(&relative))?;

    let declared = insert_above_anchor(&source, anchor::MODULES, &scaffold.module_declaration())
        .map_err(|error| format!("{}: {error}", display(&relative)))?;
    let mounted = insert_above_anchor(&declared, anchor::ROUTES, &scaffold.route_mount())
        .map_err(|error| format!("{}: {error}", display(&relative)))?;

    Ok((relative, mounted))
}

/// Grants the model to the `default` and `editor` roles in `config/roles.yml`.
fn update_roles(root: &Path, scaffold: &ModelScaffold) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("config/roles.yml");
    let mut roles = read(&root.join(&relative))?;

    for (anchor, actions) in ROLE_GRANTS {
        roles = insert_above_anchor(&roles, anchor, &scaffold.role_grant(actions))
            .map_err(|error| format!("{}: {error}", display(&relative)))?;
    }

    Ok((relative, roles))
}

/// Recompiles the frontend's permissions module from the updated roles.
fn regenerate_roles_client(root: &Path, roles: &str) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("frontend/src/roles.generated.ts");
    if !root.join(&relative).is_file() {
        return Err(format!(
            "{} is missing; it is generated from config/roles.yml and scaffolding keeps the \
             two in step",
            display(&relative),
        ));
    }

    let set = anubis::roles::RoleSet::from_yaml(roles)
        .map_err(|error| format!("the updated config/roles.yml is invalid: {error}"))?;
    Ok((relative, set.to_typescript()))
}

/// Formats the Rust files the scaffold wrote, when `rustfmt` is available.
///
/// Name-for-name transformation cannot preserve line widths: a shorter model
/// name lets a wrapped statement fit on one line again, a longer one pushes a
/// statement past the width. The formatter settles it, which is what keeps
/// `cargo fmt --check` green on untouched generated code. A missing `rustfmt`
/// is not fatal, since the output is valid Rust either way.
fn format_rust_files(root: &Path, plan: &Plan) {
    let files = plan
        .paths()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return;
    }

    match Command::new("rustfmt")
        .args(["--edition", EDITION])
        .args(&files)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(_status) => eprintln!("warning: rustfmt reported an error on the generated files"),
        Err(_error) => {
            eprintln!("warning: rustfmt is not on PATH; the generated files are unformatted");
        }
    }
}

/// Prints what was generated, and what still needs a hand.
fn report(scaffold: &ModelScaffold, plan: &Plan) {
    let ownership = scaffold.parent().map_or_else(
        || "owned by a team".to_owned(),
        |parent| format!("owned through {}", parent.pascal()),
    );
    println!("scaffolded {} ({ownership})", scaffold.model().pascal());

    println!();
    println!("created:");
    for (path, _contents) in &plan.created {
        println!("  {}", display(path));
    }
    println!("updated:");
    for (path, _contents) in &plan.updated {
        println!("  {}", display(path));
    }

    println!();
    println!(
        "Every scaffolded model carries the living template's `name` (required text) and \
         `description` (optional text) columns."
    );

    let extra = scaffold.extra_fields();
    if !extra.is_empty() {
        let names = extra
            .iter()
            .map(|field| field.name())
            .collect::<Vec<_>>()
            .join(", ");
        println!();
        println!(
            "note: the migration and schema.rs carry these fields as nullable columns and \
             nothing else does: {names}. The generated model.rs opens with a TODO naming \
             every place that still needs them."
        );
    }

    println!();
    println!("Next steps:");
    println!("  review the generated module, then boot the app to apply the migration");
    println!("  cargo test");
}

/// The application root the command was run in.
///
/// An application root holds `backend/`, `frontend/`, and `config/roles.yml`.
/// The search walks up from the working directory, so the command works from
/// anywhere inside an application; inside this repository it finds `starter/`,
/// the template host app.
fn app_root() -> Result<PathBuf, String> {
    let working_directory = std::env::current_dir()
        .map_err(|error| format!("failed to read the working directory: {error}"))?;

    working_directory
        .ancestors()
        .find(|candidate| is_app_root(candidate))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "{} is not inside an Anubis application; run this from a directory holding \
                 backend/, frontend/, and config/roles.yml (in this repository that is \
                 starter/)",
                working_directory.display(),
            )
        })
}

fn is_app_root(path: &Path) -> bool {
    path.join("backend/src").is_dir()
        && path.join("frontend").is_dir()
        && path.join("config/roles.yml").is_file()
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

/// Renders a relative path with forward slashes, the way the docs write them.
fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn fail(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::{migration_version, plant_note};

    #[test]
    fn a_version_steps_past_the_second_an_existing_migration_claims() {
        let directory = std::env::temp_dir().join(format!(
            "anubis-migration-version-{}-{}",
            std::process::id(),
            line!(),
        ));
        std::fs::create_dir_all(directory.join("2026-08-15-101112_create_projects"))
            .expect("scratch directories are creatable");

        let now = "2026-08-15T10:11:12Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("a fixed instant");
        assert_eq!(
            migration_version(&directory, now).unwrap(),
            "2026-08-15-101113",
        );

        std::fs::remove_dir_all(&directory).expect("scratch directories are removable");
    }

    #[test]
    fn a_note_lands_above_the_first_import() {
        let model = "//! The `Project` model.\n\nuse anubis::tenancy::TeamMembership;\n";
        let planted = plant_note(model, "// TODO(anubis): wire `summary`.\n").unwrap();
        assert_eq!(
            planted,
            "//! The `Project` model.\n\n// TODO(anubis): wire `summary`.\n\n\
             use anubis::tenancy::TeamMembership;\n",
        );
    }
}
