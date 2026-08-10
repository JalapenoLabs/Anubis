//! `anubis scaffold model`, `anubis scaffold field`, and
//! `anubis scaffold oauth`: generate a model's full-stack slice, grow it a
//! column at a time, and add a sign-in provider.
//!
//! The templates are application code, not framework assets: `scaffold model`
//! reads `backend/src/scaffolding/`, the migration that created the template's
//! table, the template's integration test, and the template model's pages
//! straight out of the application it is run in, rewrites every template name
//! into the target model's names, and writes the result beside them. Shared
//! files (`lib.rs`, `schema.rs`, `config/roles.yml`, `urls.ts`, `App.tsx`,
//! `AppShell.tsx`, `i18n.ts`) receive their lines above magic anchors, which is
//! idempotent, so re-running a scaffold never duplicates an insertion.
//!
//! `scaffold field` writes a migration and then edits the model's own
//! artifacts through the per-field anchors they inherited from the template.
//! Both commands share one set of insertions, so a field declared in a
//! `scaffold model` run and a field added a month later land identically.
//!
//! Everything is planned before anything is written: a missing template, a
//! missing anchor, or a module that already exists stops the command with the
//! application untouched.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anubis::scaffold::{
    Artifact, Field, FieldScaffold, JoinScaffold, LOCALE_FIELDS, ModelScaffold, ModelTemplate,
    Names, OauthScaffold, Replacements, anchor, insert_above_anchor, insert_json_entries,
    line_containing, locale_file, model_artifacts, table_block,
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

/// The page `scaffold oauth` adds a provider button to.
const SIGN_IN_PAGE: &str = "frontend/src/pages/auth/SignInPage.tsx";

/// The application's own strings, as opposed to a model's locale file.
const BASE_LOCALE: &str = "frontend/src/locales/en-US.json";

/// The object inside it holding the sign-in providers' button text.
const LOCALE_OAUTH: &str = "oauth";

/// The url helper every provider button calls, imported once.
const OAUTH_URL_IMPORT: &str = "getOauthStartUrl,";

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
    created.extend(stamp_frontend(root, template, &replacements)?);
    // The stamped files carry the template's per-field anchors, so the fields
    // beyond the template's own shape are wired exactly as `scaffold field`
    // would wire them into a model generated last month.
    wire_fields(&mut created, scaffold.model(), &scaffold.added_fields())?;

    let schema = update_schema(root, scaffold, template, &replacements)?;
    let library = update_lib(root, scaffold)?;
    let roles = update_roles(root, scaffold)?;
    let roles_client = regenerate_roles_client(root, &roles.1)?;

    let mut updated = vec![schema, library, roles, roles_client];
    updated.extend(update_frontend(root, scaffold)?);

    Ok(Plan { created, updated })
}

/// Stamps the template module into the application's own module directory.
fn stamp_module(
    root: &Path,
    scaffold: &ModelScaffold,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    stamp_template_module(root, template.module(), &scaffold.module(), replacements)
}

/// Stamps one template module directory into the application's own tree.
///
/// Shared by `scaffold model` and `scaffold join`: both transform a directory
/// under `backend/src/scaffolding/` into a module named after the target, and
/// neither ever overwrites one that already exists.
fn stamp_template_module(
    root: &Path,
    template_module: &str,
    module: &str,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    let source = PathBuf::from("backend/src/scaffolding").join(template_module);
    let destination = PathBuf::from("backend/src").join(module);
    if root.join(&destination).exists() {
        return Err(format!(
            "{} already exists; scaffolding never overwrites a model",
            display(&destination),
        ));
    }

    let entries = std::fs::read_dir(root.join(&source))
        .map_err(|error| format!("failed to read {}: {error}", display(&source)))?;

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

        let contents = replacements.apply(&read(&path)?);
        files.push((destination.join(name), contents));
    }

    if files.is_empty() {
        return Err(format!("{} holds no template files", display(&source)));
    }
    // Directory order is arbitrary; the report is not.
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
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
    let added = scaffold.added_sql_columns();
    if !added.is_empty() {
        up = insert_above_anchor(
            &up,
            "created_at TIMESTAMPTZ",
            &format!("{}\n", added.join("\n")),
        )
        .map_err(|error| format!("{}: {error}", source.join("up.sql").display()))?;
    }
    let down = replacements.apply(&read(&source.join("down.sql"))?);

    Ok(vec![
        (destination.join("up.sql"), up),
        (destination.join("down.sql"), down),
    ])
}

/// Stamps the template model's frontend files into the application.
///
/// A team-owned model gets its route module, its form, its locale file, and
/// its two pages; a nested model gets its route module, its form, its locale
/// file, and the section component its parent's page renders. Template-only
/// lines are dropped: the template's parent page renders the template's own
/// child, which belongs to no other model.
fn stamp_frontend(
    root: &Path,
    template: ModelTemplate,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    stamp_frontend_files(root, template.frontend_files(), replacements)
}

/// Stamps a list of template frontend files into the application.
fn stamp_frontend_files(
    root: &Path,
    sources: &[&str],
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut files = Vec::new();
    for source in sources {
        let destination = replacements.apply(source);
        if root.join(&destination).exists() {
            return Err(format!(
                "{destination} already exists; scaffolding never overwrites a model",
            ));
        }

        let stamped = replacements.apply(&read(&root.join(source))?);
        files.push((PathBuf::from(destination), drop_template_only(&stamped)));
    }

    Ok(files)
}

/// Removes the lines that belong to the living template alone.
fn drop_template_only(contents: &str) -> String {
    let mut kept = String::with_capacity(contents.len());
    for line in contents.lines() {
        if line.contains(anchor::TEMPLATE_ONLY) {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// Wires the model's frontend slice into the files the application shares.
///
/// Every model registers its locale file. Beyond that the two depths differ:
/// a team-owned model owns urls, routes, and a navigation entry, while a
/// nested model attaches a section to the show page its parent's scaffold
/// generated.
fn update_frontend(
    root: &Path,
    scaffold: &ModelScaffold,
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut updated = vec![update_anchors(
        root,
        "frontend/src/i18n.ts",
        &[
            (anchor::LOCALE_IMPORTS, scaffold.locale_import()),
            (anchor::LOCALES, scaffold.locale_spread()),
        ],
    )?];

    if let Some(attachment) = scaffold.child_attachment() {
        if !root.join(&attachment.page).is_file() {
            return Err(format!(
                "{} is missing; scaffold the parent model before the models it owns, so there \
                 is a show page for this one to attach to",
                attachment.page,
            ));
        }
        updated.push(update_anchors(
            root,
            &attachment.page,
            &[
                (anchor::CHILD_IMPORTS, attachment.import),
                (anchor::CHILDREN, attachment.element),
            ],
        )?);
        return Ok(updated);
    }

    updated.push(update_anchors(
        root,
        "frontend/src/urls.ts",
        &[
            (anchor::URLS, scaffold.url_entries()),
            (anchor::URL_FACTORIES, scaffold.url_factory()),
        ],
    )?);
    updated.push(update_anchors(
        root,
        "frontend/src/App.tsx",
        &[
            (anchor::PAGE_IMPORTS, scaffold.page_imports()),
            (anchor::ROUTES, scaffold.route_elements()),
        ],
    )?);
    updated.push(update_anchors(
        root,
        "frontend/src/components/AppShell.tsx",
        &[(anchor::NAV, scaffold.nav_item())],
    )?);

    Ok(updated)
}

/// Applies anchor insertions to one file the application already owns.
fn update_anchors(
    root: &Path,
    relative: &str,
    insertions: &[(&str, String)],
) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from(relative);
    let mut contents = read(&root.join(&relative))?;
    for (anchor, addition) in insertions {
        contents = insert_above_anchor(&contents, anchor, addition)
            .map_err(|error| format!("{}: {error}", display(&relative)))?;
    }
    Ok((relative, contents))
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
    let added = scaffold.added_schema_columns();
    if !added.is_empty() {
        block = insert_above_anchor(&block, "created_at ->", &format!("{}\n", added.join("\n")))
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
    update_anchors(
        root,
        "backend/src/lib.rs",
        &[
            (anchor::MODULES, scaffold.module_declaration()),
            (anchor::ROUTES, scaffold.route_mount()),
        ],
    )
}

/// Grants the model to the `default` and `editor` roles in `config/roles.yml`.
fn update_roles(root: &Path, scaffold: &ModelScaffold) -> Result<(PathBuf, String), String> {
    let grants = ROLE_GRANTS.map(|(anchor, actions)| (anchor, scaffold.role_grant(actions)));
    update_anchors(root, "config/roles.yml", &grants)
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

    let added = scaffold.added_fields();
    if !added.is_empty() {
        let names = added
            .iter()
            .map(|field| format!("{} ({})", field.name(), field.field().type_name()))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "Wired end to end alongside them: {names}. Added columns are nullable, or \
             non-null with a database default, so a later migration never strands \
             existing rows."
        );
    }

    println!();
    println!("Next steps:");
    println!("  review the generated module, then boot the app to apply the migration");
    println!("  cargo test");
}

/// Runs `anubis scaffold field <Model> <field:type>`.
pub(crate) fn field(model: &str, argument: &str) -> ExitCode {
    let names = match Names::parse(model) {
        Ok(names) => names,
        Err(error) => return fail(&error.to_string()),
    };
    let parsed = match Field::parse(argument) {
        Ok(parsed) => parsed,
        Err(error) => return fail(error.message()),
    };

    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };

    // An association reads and writes through a join model, and only the
    // application knows which one links the pair.
    let parsed = match parsed.association() {
        None => parsed,
        Some(association) => match resolve_join(&root, &names, association.target()) {
            Ok(join) => parsed.through(join),
            Err(reason) => return fail(&reason),
        },
    };
    let scaffold = FieldScaffold::new(names.clone(), parsed);

    let plan = match plan_field(&root, &names, &scaffold) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };
    if let Err(reason) = plan.plan.apply(&root) {
        return fail(&reason);
    }

    report_field(&names, &scaffold, &plan);
    ExitCode::SUCCESS
}

/// One planned `scaffold field` run: what it writes, and what it could not find.
struct FieldPlan {
    plan: Plan,
    /// Artifacts the model does not have, named so nothing is skipped quietly.
    absent: Vec<String>,
}

/// Plans the migration, the schema column, and every artifact insertion.
fn plan_field(root: &Path, names: &Names, scaffold: &FieldScaffold) -> Result<FieldPlan, String> {
    let module = names.snake_plural();
    let table = names.snake_plural();
    if !root
        .join(format!("backend/src/{module}/model.rs"))
        .is_file()
    {
        return Err(format!(
            "no model named `{}` in this application: backend/src/{module}/model.rs does not \
             exist. Generate the model first with `anubis scaffold model {} Team`.",
            names.pascal(),
            names.pascal(),
        ));
    }

    // An association adds no column to this model's table: its values are rows
    // in the join table, so the run writes no migration and touches no schema.
    let mut created = Vec::new();
    let mut updated = Vec::new();
    if scaffold.field().field_type().is_some() {
        updated.push(update_schema_column(root, &table, scaffold)?);
        created = migration_for_field(root, &table, scaffold)?;
    } else {
        require_association_absent(root, &module, scaffold)?;
    }

    let fields = std::slice::from_ref(scaffold);
    let mut absent = Vec::new();
    for (relative, artifact) in model_artifacts(names) {
        let path = root.join(&relative);
        if !path.is_file() {
            absent.push(relative);
            continue;
        }
        let contents = insert_field(&relative, &read(&path)?, artifact, fields)?;
        updated.push((PathBuf::from(relative), contents));
    }

    let locale = locale_file(names);
    if root.join(&locale).is_file() {
        let contents = insert_locale(&locale, &read(&root.join(&locale))?, names, fields)?;
        updated.push((PathBuf::from(locale), contents));
    } else {
        absent.push(locale);
    }

    Ok(FieldPlan {
        plan: Plan { created, updated },
        absent,
    })
}

/// Adds the column to the model's own `diesel::table!` block.
///
/// The block carries no anchor, because one spelling may appear only once per
/// file and `schema.rs` holds a block per model. Its structure is the
/// insertion point instead: the column joins the others, above the timestamps
/// the database maintains, matching the block's indentation.
fn update_schema_column(
    root: &Path,
    table: &str,
    scaffold: &FieldScaffold,
) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("backend/src/schema.rs");
    let schema = read(&root.join(&relative))?;

    let block = table_block(&schema, table).ok_or_else(|| {
        format!(
            "{} declares no `{table}` table, so there is no column list to add to",
            display(&relative),
        )
    })?;
    let declaration = format!("{} ->", scaffold.name());
    if block
        .lines()
        .any(|line| line.trim_start().starts_with(&declaration))
    {
        return Err(format!(
            "`{table}` already has a `{}` column; scaffolding never redefines a field",
            scaffold.name(),
        ));
    }

    let column = scaffold.field().schema_column().ok_or_else(|| {
        format!(
            "`{}` adds no column, so it belongs in no table block",
            scaffold.name(),
        )
    })?;
    let updated_block =
        insert_above_anchor(&block, "created_at ->", &column).map_err(|_error| {
            format!(
                "the `{table}` block in {} has no `created_at` column to insert above",
                display(&relative),
            )
        })?;
    Ok((relative, schema.replace(&block, &updated_block)))
}

/// The join backing an association, or the command that would create it.
fn resolve_join(root: &Path, model: &Names, target: &Names) -> Result<Names, String> {
    match find_join(root, model, target)? {
        Some(join) => Ok(join),
        None => Err(format!(
            "no join model links {} to {}. A has-many-through association reads through one, so \
             generate it first:\n  anubis scaffold join <JoinModel> {}_id{{class_name={}}} \
             {}_id{{class_name={}}}",
            model.pascal(),
            target.pascal(),
            model.snake(),
            model.pascal(),
            target.snake(),
            target.pascal(),
        )),
    }
}

/// Refuses an association the model already carries.
///
/// An association owns no column, so the schema cannot answer the question the
/// way it does for a field; the request body the earlier run wrote does.
fn require_association_absent(
    root: &Path,
    module: &str,
    scaffold: &FieldScaffold,
) -> Result<(), String> {
    let relative = format!("backend/src/{module}/routes.rs");
    let declaration = format!("{}: Option<Vec<Uuid>>,", scaffold.name());
    if read(&root.join(&relative))?.contains(&declaration) {
        return Err(format!(
            "`{module}` already carries the `{}` association; scaffolding never redefines a field",
            scaffold.name(),
        ));
    }
    Ok(())
}

/// Writes the `ALTER TABLE` migration that adds the column, and drops it again.
fn migration_for_field(
    root: &Path,
    table: &str,
    scaffold: &FieldScaffold,
) -> Result<Vec<(PathBuf, String)>, String> {
    let migrations = root.join("backend/migrations");
    let version = migration_version(&migrations, chrono::Utc::now())?;
    let destination = PathBuf::from("backend/migrations")
        .join(format!("{version}_add_{}_to_{table}", scaffold.name()));

    let (added, dropped) = scaffold
        .add_column(table)
        .zip(scaffold.drop_column(table))
        .ok_or_else(|| {
            format!(
                "`{}` adds no column, so it needs no migration",
                scaffold.name(),
            )
        })?;

    Ok(vec![
        (destination.join("up.sql"), format!("{added}\n")),
        (destination.join("down.sql"), format!("{dropped}\n")),
    ])
}

/// Applies every field's insertions to the artifacts among `files`.
///
/// `scaffold model` calls this on the files it has just stamped, which is what
/// makes an extra field on a new model identical to a field added later.
fn wire_fields(
    files: &mut [(PathBuf, String)],
    model: &Names,
    fields: &[FieldScaffold],
) -> Result<(), String> {
    if fields.is_empty() {
        return Ok(());
    }

    let artifacts = model_artifacts(model);
    let locale = locale_file(model);
    for (path, contents) in files {
        let relative = display(path);
        if relative == locale {
            *contents = insert_locale(&relative, contents, model, fields)?;
            continue;
        }
        if let Some((_path, artifact)) = artifacts
            .iter()
            .find(|(candidate, _artifact)| *candidate == relative)
        {
            *contents = insert_field(&relative, contents, *artifact, fields)?;
        }
    }
    Ok(())
}

/// Inserts every field's lines above the anchors one artifact carries.
fn insert_field(
    relative: &str,
    contents: &str,
    artifact: Artifact,
    fields: &[FieldScaffold],
) -> Result<String, String> {
    let mut updated = contents.to_owned();
    for field in fields {
        for (anchor, lines) in field.insertions(artifact) {
            updated = insert_above_anchor(&updated, anchor, &lines)
                .map_err(|_error| missing_anchor(relative, anchor))?;
        }
    }
    Ok(updated)
}

/// Adds every field's label and help text to the model's locale file.
fn insert_locale(
    relative: &str,
    contents: &str,
    model: &Names,
    fields: &[FieldScaffold],
) -> Result<String, String> {
    let entries = fields
        .iter()
        .flat_map(FieldScaffold::locale_entries)
        .collect::<Vec<_>>();
    insert_json_entries(contents, LOCALE_FIELDS, &entries).ok_or_else(|| {
        format!(
            "{relative} holds no `{LOCALE_FIELDS}` object under `{}` to add the field's strings to",
            model.camel_plural(),
        )
    })
}

/// The message a file that lost an anchor comment earns.
fn missing_anchor(relative: &str, anchor: &str) -> String {
    format!(
        "{relative} no longer carries the anchor `{anchor}`. Scaffolding inserts a field's \
         lines above it, so restore the anchor comment and run this again.",
    )
}

/// Prints what one `scaffold field` run changed.
fn report_field(names: &Names, scaffold: &FieldScaffold, plan: &FieldPlan) {
    println!(
        "added {} ({}) to {}",
        scaffold.name(),
        scaffold.field().type_name(),
        names.pascal(),
    );

    println!();
    println!("created:");
    for (path, _contents) in &plan.plan.created {
        println!("  {}", display(path));
    }
    println!("updated:");
    for (path, _contents) in &plan.plan.updated {
        println!("  {}", display(path));
    }
    if !plan.absent.is_empty() {
        println!("not found, so not updated:");
        for relative in &plan.absent {
            println!("  {relative}");
        }
    }

    println!();
    match scaffold.field().field_type() {
        Some(field_type) if field_type.is_nullable() => {
            println!("The column is nullable, so the rows the table already holds stay valid.");
        }
        Some(_defaulted) => {
            println!(
                "The column is NOT NULL with a database default, so the rows the table \
                 already holds stay valid."
            );
        }
        None => {
            println!(
                "The association adds no column: its values are rows in the join table, which \
                 already exists, so this run writes no migration."
            );
        }
    }
    println!();
    println!("Next steps:");
    if scaffold.field().field_type().is_some() {
        println!("  boot the app to apply the migration");
    }
    println!("  cargo test");
}

/// Runs `anubis scaffold oauth <provider>`.
///
/// The command is thin on purpose: the flow, the routes, and the identity
/// linking are framework behavior that arrives with the dependency, so all
/// that is generated is the part the application owns. What it cannot do for
/// you, registering the client with the provider, it prints.
pub(crate) fn oauth(provider: &str) -> ExitCode {
    let Some(known) = anubis::auth::oauth::find_provider(provider) else {
        return fail(&unknown_provider(provider));
    };
    let scaffold = OauthScaffold::new(known);

    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };

    let plan = match plan_oauth(&root, scaffold) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };
    if let Err(reason) = plan.apply(&root) {
        return fail(&reason);
    }

    report_oauth(scaffold, &plan);
    ExitCode::SUCCESS
}

/// Plans the button and the string one provider adds.
fn plan_oauth(root: &Path, scaffold: OauthScaffold) -> Result<Plan, String> {
    // The import is one line whatever the provider, and anchor insertion is
    // idempotent, so a second provider adds a button and nothing else.
    let page = update_anchors(
        root,
        SIGN_IN_PAGE,
        &[
            (anchor::OAUTH_IMPORTS, OAUTH_URL_IMPORT.to_owned()),
            (anchor::OAUTH_PROVIDERS, scaffold.sign_in_button()),
        ],
    )?;

    let relative = PathBuf::from(BASE_LOCALE);
    let locale = read(&root.join(&relative))?;
    let updated = insert_json_entries(&locale, LOCALE_OAUTH, &scaffold.locale_entries())
        .ok_or_else(|| {
            format!(
                "{BASE_LOCALE} holds no `{LOCALE_OAUTH}` object under `auth` to add the \
                 provider's button text to",
            )
        })?;

    Ok(Plan {
        created: Vec::new(),
        updated: vec![page, (relative, updated)],
    })
}

/// The message an unknown provider key earns, with the boundary spelled out.
fn unknown_provider(provider: &str) -> String {
    let known = anubis::auth::oauth::known_providers()
        .iter()
        .map(|provider| provider.key)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "`{provider}` is not a known sign-in provider. Anubis speaks OpenID Connect, and knows: \
         {known}. GitHub issues no ID token and publishes no discovery document, so it needs a \
         plain OAuth 2 path the framework does not have yet.",
    )
}

/// Prints what changed, and the two steps only the developer can take.
fn report_oauth(scaffold: OauthScaffold, plan: &Plan) {
    let provider = scaffold.provider();
    println!("added {} sign-in", provider.display_name);

    println!();
    println!("updated:");
    for (path, _contents) in &plan.updated {
        println!("  {}", display(path));
    }

    // The public origin is what the provider redirects the browser back to,
    // so the example is only useful with the value this app actually runs on.
    let app_url = std::env::var("APP_URL").unwrap_or_else(|_error| "<APP_URL>".to_owned());

    println!();
    println!("Next steps:");
    println!(
        "  register an OAuth client with {}, with this redirect URI:",
        provider.display_name,
    );
    println!("    {}", scaffold.redirect_uri(&app_url));
    println!("  set both credentials in the environment:");
    println!("    {}=...", provider.client_id_var);
    println!("    {}=...", provider.client_secret_var);
    println!(
        "  APP_URL must be the origin the browser sees, since it is the base of that redirect URI"
    );
}

/// Runs `anubis scaffold join <JoinModel> <a_id{class_name=A}> <b_id{class_name=B}>`.
pub(crate) fn join(join: &str, owner: &str, target: &str) -> ExitCode {
    let scaffold = match JoinScaffold::parse(join, owner, target) {
        Ok(scaffold) => scaffold,
        Err(error) => return fail(error.message()),
    };

    let root = match app_root() {
        Ok(root) => root,
        Err(reason) => return fail(&reason),
    };

    let plan = match plan_join(&root, &scaffold) {
        Ok(plan) => plan,
        Err(reason) => return fail(&reason),
    };
    if let Err(reason) = plan.apply(&root) {
        return fail(&reason);
    }

    report_join(&scaffold, &plan);
    ExitCode::SUCCESS
}

/// Plans the whole join, reading the application's own join template.
fn plan_join(root: &Path, scaffold: &JoinScaffold) -> Result<Plan, String> {
    require_team_owned(root, scaffold.owner())?;
    require_team_owned(root, scaffold.target())?;
    if let Some(existing) = find_join(root, scaffold.owner(), scaffold.target())? {
        return Err(format!(
            "`{}` already links {} and {}; one join model per pair of models",
            existing.pascal(),
            scaffold.owner().pascal(),
            scaffold.target().pascal(),
        ));
    }

    let template = scaffold.template();
    let replacements = scaffold.replacements();

    let mut created =
        stamp_template_module(root, template.module(), &scaffold.module(), &replacements)?;
    created.push(stamp_join_test(root, scaffold, &replacements)?);
    created.extend(stamp_join_migration(root, scaffold, &replacements)?);
    created.extend(stamp_frontend_files(
        root,
        template.frontend_files(),
        &replacements,
    )?);

    let schema = update_join_schema(root, scaffold, &replacements)?;
    let library = update_anchors(
        root,
        "backend/src/lib.rs",
        &[
            (anchor::MODULES, scaffold.module_declaration()),
            (anchor::ROUTES, scaffold.route_mount()),
        ],
    )?;

    Ok(Plan {
        created,
        updated: vec![schema, library],
    })
}

/// Refuses a side that is not a model this application owns a team-owned slice for.
///
/// A join links two models that both reach the same team directly, which is
/// what lets one comparison decide whether a pair is tenant-safe. A model owned
/// through a parent is refused by name rather than generated half-scoped.
fn require_team_owned(root: &Path, names: &Names) -> Result<(), String> {
    let module = names.snake_plural();
    let relative = format!("backend/src/{module}/model.rs");
    if !root.join(&relative).is_file() {
        return Err(format!(
            "no model named `{}` in this application: {relative} does not exist. Generate it \
             first with `anubis scaffold model {} Team`.",
            names.pascal(),
            names.pascal(),
        ));
    }
    if !read(&root.join(&relative))?.contains("pub team_id: Uuid,") {
        return Err(format!(
            "`{}` is not owned directly by a team. A join links two team-owned models today, so \
             both sides reach the same team in one step; deeper chains are on the roadmap.",
            names.pascal(),
        ));
    }
    Ok(())
}

/// The join model linking `owner` to `target`, if the application has one.
///
/// Join models declare themselves with `pub const JOIN`, so the lookup is a
/// scan of the application's own models rather than a naming convention the
/// developer has to remember.
fn find_join(root: &Path, owner: &Names, target: &Names) -> Result<Option<Names>, String> {
    let source = root.join("backend/src");
    for module in directory_names(&source)? {
        let model = source.join(&module).join("model.rs");
        if !model.is_file() {
            continue;
        }
        let Some([join, declared_owner, declared_target]) = join_declaration(&read(&model)?) else {
            continue;
        };
        if declared_owner == owner.pascal() && declared_target == target.pascal() {
            return Ok(Some(Names::parse(&join).map_err(|error| {
                format!(
                    "{} declares an unparseable join name: {error}",
                    model.display()
                )
            })?));
        }
    }
    Ok(None)
}

/// The three model names a join model's `JOIN` constant declares.
fn join_declaration(contents: &str) -> Option<[String; 3]> {
    const DECLARATION: &str = "pub const JOIN: [&str; 3] = [";

    let start = contents.find(DECLARATION)?;
    let rest = &contents[start..];
    let end = rest.find("];")?;
    let names = rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.try_into().ok()
}

/// Stamps the join template's narrative test into the application's suite.
fn stamp_join_test(
    root: &Path,
    scaffold: &JoinScaffold,
    replacements: &Replacements,
) -> Result<(PathBuf, String), String> {
    let tests = PathBuf::from("backend/tests");
    let source = tests.join(format!("{}_flow.rs", scaffold.template().table()));
    let destination = tests.join(format!("{}_flow.rs", scaffold.table()));
    if root.join(&destination).exists() {
        return Err(format!(
            "{} already exists; move it aside to scaffold this join again",
            display(&destination),
        ));
    }

    let contents = replacements.apply(&read(&root.join(&source))?);
    Ok((destination, contents))
}

/// Stamps the migration that created the join template's table.
fn stamp_join_migration(
    root: &Path,
    scaffold: &JoinScaffold,
    replacements: &Replacements,
) -> Result<Vec<(PathBuf, String)>, String> {
    let migrations = root.join("backend/migrations");
    let source = template_migration(&migrations, &scaffold.template().table())?;
    let version = migration_version(&migrations, chrono::Utc::now())?;
    let destination =
        PathBuf::from("backend/migrations").join(scaffold.migration_directory(&version));

    Ok(vec![
        (
            destination.join("up.sql"),
            replacements.apply(&read(&source.join("up.sql"))?),
        ),
        (
            destination.join("down.sql"),
            replacements.apply(&read(&source.join("down.sql"))?),
        ),
    ])
}

/// Adds the join's table, both joins, and both same-query pairs to `schema.rs`.
fn update_join_schema(
    root: &Path,
    scaffold: &JoinScaffold,
    replacements: &Replacements,
) -> Result<(PathBuf, String), String> {
    let relative = PathBuf::from("backend/src/schema.rs");
    let schema = read(&root.join(&relative))?;
    let template = scaffold.template();

    let block = table_block(&schema, &template.table()).ok_or_else(|| {
        format!(
            "{} declares no `{}` table; the living template's table is what a scaffold transforms",
            display(&relative),
            template.table(),
        )
    })?;
    let mut updated = insert_above_anchor(
        &schema,
        anchor::TABLES,
        &format!("{}\n\n", replacements.apply(&block)),
    )
    .map_err(|error| format!("{}: {error}", display(&relative)))?;

    // A join declares one `joinable!` per side and one same-query pair per
    // side; the pair of sides never meets in a query, so it needs neither.
    let join_table = template.table();
    for (needle, anchor) in [
        (
            format!(
                "diesel::joinable!({join_table} -> {}",
                template.owner_table()
            ),
            anchor::JOINS,
        ),
        (
            format!(
                "diesel::joinable!({join_table} -> {}",
                template.target_table()
            ),
            anchor::JOINS,
        ),
        (
            format!(
                "diesel::allow_tables_to_appear_in_same_query!({}, {join_table}",
                template.owner_table()
            ),
            anchor::SAME_QUERY,
        ),
        (
            format!(
                "diesel::allow_tables_to_appear_in_same_query!({}, {join_table}",
                template.target_table()
            ),
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

    Ok((relative, updated))
}

/// Prints what the join generated, and what it deliberately did not.
fn report_join(scaffold: &JoinScaffold, plan: &Plan) {
    println!(
        "scaffolded {} ({} and {})",
        scaffold.join().pascal(),
        scaffold.owner().pascal(),
        scaffold.target().pascal(),
    );

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
        "A join model is infrastructure, not a resource, so it takes no entry in \
         config/roles.yml: reading the association is a read on {}, changing it is an update \
         on {}, and listing a form's options is a read on {}.",
        scaffold.owner().pascal(),
        scaffold.owner().pascal(),
        scaffold.target().pascal(),
    );

    println!();
    println!("Next steps:");
    println!("  boot the app to apply the migration");
    println!(
        "  anubis scaffold field {} {}_ids:super_select{{class_name={}}}",
        scaffold.owner().pascal(),
        scaffold.target().snake(),
        scaffold.target().pascal(),
    );
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
    use anubis::scaffold::{Artifact, Field, FieldScaffold, Names};

    use super::{insert_field, migration_version, missing_anchor};

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
    fn a_file_that_lost_an_anchor_is_named_with_it() {
        let scaffold = FieldScaffold::new(
            Names::parse("Project").unwrap(),
            Field::parse("priority:text_field").unwrap(),
        );
        let customized = "pub struct Project {\n    pub id: Uuid,\n}\n";
        let error = insert_field(
            "backend/src/projects/model.rs",
            customized,
            Artifact::Model,
            std::slice::from_ref(&scaffold),
        )
        .unwrap_err();
        assert!(error.contains("backend/src/projects/model.rs"), "{error}");
        assert!(error.contains("🐺 anubis:record-fields"), "{error}");
        assert!(error.contains("restore the anchor"), "{error}");

        assert_eq!(
            missing_anchor("a.rs", "🐺 anubis:x"),
            "a.rs no longer carries the anchor `🐺 anubis:x`. Scaffolding inserts a field's \
             lines above it, so restore the anchor comment and run this again.",
        );
    }

    #[test]
    fn an_artifact_gains_every_line_the_field_owes_it() {
        let scaffold = FieldScaffold::new(
            Names::parse("Project").unwrap(),
            Field::parse("priority:text_field").unwrap(),
        );
        let model = "\
pub struct Project {
    pub name: String,
    // 🐺 anubis:record-fields
}

pub struct NewProject<'a> {
    pub name: &'a str,
    // 🐺 anubis:insert-fields
}

pub struct ProjectChanges {
    pub name: Option<String>,
    // 🐺 anubis:changeset-fields
}

impl ProjectChanges {
    pub fn is_empty(&self) -> bool {
        // 🐺 anubis:changeset-empty
        true
    }
}
";
        let updated = insert_field(
            "backend/src/projects/model.rs",
            model,
            Artifact::Model,
            std::slice::from_ref(&scaffold),
        )
        .unwrap();
        assert!(updated.contains("    pub priority: Option<String>,\n"));
        assert!(updated.contains("    pub priority: Option<&'a str>,\n"));
        assert!(updated.contains("    pub priority: Option<Option<String>>,\n"));
        assert!(
            updated.contains("        if self.priority.is_some() {\n            return false;")
        );

        // Running the same insertion twice changes nothing.
        assert_eq!(
            insert_field(
                "backend/src/projects/model.rs",
                &updated,
                Artifact::Model,
                std::slice::from_ref(&scaffold),
            )
            .unwrap(),
            updated,
        );
    }
}
