//! `anubis scaffold` against a real copy of the starter application.
//!
//! The scaffolder's inputs are files the application owns, so the only honest
//! test is to run the real binary in a real application tree: the starter is
//! copied to a scratch directory, models and fields are scaffolded into it,
//! and the result is inspected as text. Compiling the copy is left to the
//! workspace's own `cargo test`, which builds the starter after a scaffold
//! during development, and to the frontend toolchain, which typechecks and
//! lints it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANUBIS: &str = env!("CARGO_BIN_EXE_anubis");

/// Directories that are build output rather than application source.
const SKIPPED: [&str; 5] = ["target", "node_modules", "dist", ".vite", "coverage"];

/// Names the living templates use that must never survive a transformation.
const TEMPLATE_TOKENS: [&str; 11] = [
    "CreativeConcept",
    "creativeConcept",
    "creative_concept",
    "CREATIVE_CONCEPT",
    "creative-concept",
    "TangibleThing",
    "tangibleThing",
    "tangible_thing",
    "TANGIBLE_THING",
    "absolutely_abstract",
    "completely_concrete",
];

/// The names the join template adds, which must not survive either.
const JOIN_TEMPLATE_TOKENS: [&str; 8] = [
    "IncidentalLinkage",
    "incidentalLinkage",
    "incidental_linkage",
    "PeripheralNotion",
    "peripheralNotion",
    "peripheral_notion",
    "incidentally_linked",
    "merely_peripheral",
];

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scaffold run inspected artifact by artifact"
)]
fn scaffolding_two_models_writes_a_full_stack_slice() {
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
        "frontend/src/api/routes/projectRoutes.ts",
        "frontend/src/components/ProjectForm.tsx",
        "frontend/src/pages/ProjectPage.tsx",
        "frontend/src/pages/ProjectsPage.tsx",
        "frontend/src/locales/models/projects.en-US.json",
        "frontend/src/api/routes/goalRoutes.ts",
        "frontend/src/components/GoalForm.tsx",
        "frontend/src/components/GoalsSection.tsx",
        "frontend/src/locales/models/goals.en-US.json",
    ] {
        assert!(app.join(expected).is_file(), "missing {expected}");
    }
    // A nested model owns no pages of its own.
    assert!(!app.join("frontend/src/pages/GoalPage.tsx").exists());
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
        "frontend/src/api/routes/projectRoutes.ts",
        "frontend/src/api/routes/goalRoutes.ts",
        "frontend/src/components/ProjectForm.tsx",
        "frontend/src/components/GoalsSection.tsx",
        "frontend/src/pages/ProjectPage.tsx",
        "frontend/src/pages/ProjectsPage.tsx",
        "frontend/src/locales/models/projects.en-US.json",
        "frontend/src/locales/models/goals.en-US.json",
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

    // A team-owned model owns its urls, its routes, and a navigation entry.
    let urls = read(&app.join("frontend/src/urls.ts"));
    assert!(urls.contains("projects: '/projects',"), "{urls}");
    assert!(urls.contains("project: '/projects/:projectId',"));
    assert!(urls.contains("export function getProjectUrl(projectId: string): string {"));
    assert_anchored(&urls, "🐺 anubis:urls", "projects: '/projects',");
    assert_anchored(&urls, "🐺 anubis:url-factories", "getProjectUrl");

    let application = read(&app.join("frontend/src/App.tsx"));
    assert!(
        application.contains("import { ProjectsPage } from './pages/ProjectsPage'"),
        "{application}",
    );
    assert!(application.contains("path={UrlTree.project}"));
    assert_anchored(&application, "🐺 anubis:page-imports", "ProjectsPage }");
    assert_anchored(&application, "🐺 anubis:routes", "<ProjectsPage />");

    let shell = read(&app.join("frontend/src/components/AppShell.tsx"));
    assert!(shell.contains("t('projects.navLink')"), "{shell}");
    assert_anchored(&shell, "🐺 anubis:nav", "UrlTree.projects");

    let i18n = read(&app.join("frontend/src/i18n.ts"));
    assert!(i18n.contains("import goalsEnUS from './locales/models/goals.en-US.json'"));
    assert!(i18n.contains("...projectsEnUS,"), "{i18n}");
    assert_anchored(&i18n, "🐺 anubis:locale-imports", "projectsEnUS from");
    assert_anchored(&i18n, "🐺 anubis:locales", "...goalsEnUS,");

    // The generated form renders the field components, one per attribute.
    let form = read(&app.join("frontend/src/components/ProjectForm.tsx"));
    assert!(form.contains("} from '@jalapenolabs/anubis'"), "{form}");
    assert!(form.contains("  TextAreaField,\n  TextField,\n"));
    assert!(form.contains("label={t('projects.fields.name')}"));
    assert!(form.contains("help={t('projects.fields.descriptionHelp')}"));

    // The nested model attaches to the page the parent's own scaffold wrote,
    // and the template's own child is not carried along with it.
    let page = read(&app.join("frontend/src/pages/ProjectPage.tsx"));
    assert!(
        page.contains("<GoalsSection projectId={projectId} />"),
        "{page}"
    );
    assert!(!page.contains("🐺 anubis:template-only"), "{page}");
    assert_anchored(&page, "🐺 anubis:children", "<GoalsSection");
    assert_anchored(&page, "🐺 anubis:child-imports", "import { GoalsSection }");

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

    let orphan = scaffold(&app, &["Task", "Missing,Team"]);
    assert!(!orphan.status.success());
    assert!(
        stderr(&orphan).contains("frontend/src/pages/MissingPage.tsx is missing"),
        "a nested model needs its parent's page to attach to: {}",
        stderr(&orphan),
    );

    let unsupported = scaffold(&app, &["Task", "Team", "shade:color_picker"]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("text_field, text_area, number_field, boolean, date_field"),
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

/// A field added to a model that already exists reaches every artifact, and
/// the artifacts a scaffolded model carries are the ones that receive it.
#[test]
fn a_field_added_later_reaches_every_artifact() {
    let app = copy_starter("field");

    let output = scaffold(&app, &["Project", "Team", "name:text_field"]);
    assert!(
        output.status.success(),
        "scaffolding Project failed: {}",
        stderr(&output),
    );

    let output = scaffold_field(&app, &["Project", "priority:text_field"]);
    assert!(
        output.status.success(),
        "scaffolding the field failed: {}",
        stderr(&output),
    );

    // A timestamped migration adds the column, and gives it back.
    let added = migration(&app, "_add_priority_to_projects");
    assert_eq!(
        read(&added.join("up.sql")).trim(),
        "ALTER TABLE projects ADD COLUMN priority TEXT;",
    );
    assert_eq!(
        read(&added.join("down.sql")).trim(),
        "ALTER TABLE projects DROP COLUMN priority;",
    );

    // The column joins the model's own table block, above the timestamps the
    // database maintains, and no other model's block moves.
    let schema = read(&app.join("backend/src/schema.rs"));
    assert!(
        schema.contains("        priority -> Nullable<Text>,\n        created_at -> Timestamptz,"),
        "{schema}",
    );
    assert_eq!(schema.matches("priority ->").count(), 1);

    // The backend: record, insertable, changeset, the emptiness test, both
    // request bodies, both normalizations, and both struct literals.
    let model = read(&app.join("backend/src/projects/model.rs"));
    assert!(model.contains("pub priority: Option<String>,"), "{model}");
    assert!(model.contains("pub priority: Option<&'a str>,"));
    assert!(model.contains("pub priority: Option<Option<String>>,"));
    assert!(model.contains("if self.priority.is_some() {"));

    let routes = read(&app.join("backend/src/projects/routes.rs"));
    assert_eq!(
        routes.matches("priority: Option<String>,").count(),
        2,
        "both request bodies carry the field: {routes}",
    );
    assert!(routes.contains("let priority = optional_text(body.priority.as_deref());"));
    assert!(routes.contains(".map(str::trim)"), "{routes}");
    assert_eq!(
        routes.matches("        priority,\n").count(),
        2,
        "the insertable and the changeset both take the value: {routes}",
    );

    // The generated test asserts the column through the create and the update.
    let test = read(&app.join("backend/tests/projects_flow.rs"));
    assert!(test.contains("\"priority\": \"Alpha\","), "{test}");
    assert!(test.contains("assert_eq!(body[\"project\"][\"priority\"], json!(\"Alpha\"));"));
    assert!(test.contains("\"priority\": \"Beta\","));
    assert!(test.contains("assert_eq!(body[\"project\"][\"priority\"], json!(\"Beta\"));"));

    // The frontend: wire type, both request types, the form, and the table.
    let api = read(&app.join("frontend/src/api/routes/projectRoutes.ts"));
    assert!(api.contains("  priority: string | null\n"), "{api}");
    assert_eq!(api.matches("priority?: string").count(), 2);

    let form = read(&app.join("frontend/src/components/ProjectForm.tsx"));
    assert!(form.contains("priority: z.string(),"), "{form}");
    assert!(form.contains("priority: editing?.priority ?? '',"));
    assert!(form.contains("priority: data.priority.trim(),"));
    assert!(form.contains("name='priority'"));
    assert!(form.contains("label={t('projects.fields.priority')}"));

    let list = read(&app.join("frontend/src/pages/ProjectsPage.tsx"));
    assert!(list.contains("t('projects.fields.priority')"), "{list}");
    assert!(list.contains("project.priority"));

    let show = read(&app.join("frontend/src/pages/ProjectPage.tsx"));
    assert!(show.contains("record?.priority"), "{show}");

    let locale = read(&app.join("frontend/src/locales/models/projects.en-US.json"));
    assert!(locale.contains("\"priority\": \"Priority\""), "{locale}");
    assert!(locale.contains("\"priorityHelp\": \"Priority of the project.\""));
    serde_json::from_str::<serde_json::Value>(&locale).expect("the locale file stays valid JSON");

    // The same field twice refuses, and says why.
    let again = scaffold_field(&app, &["Project", "priority:text_field"]);
    assert!(!again.status.success(), "a repeated field must refuse");
    assert!(
        stderr(&again).contains("already has a `priority` column"),
        "unexpected error: {}",
        stderr(&again),
    );

    // A model that was never scaffolded is named, with the file looked for.
    let missing = scaffold_field(&app, &["Ghost", "note:text_field"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("backend/src/ghosts/model.rs"),
        "unexpected error: {}",
        stderr(&missing),
    );

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// A file a developer customized past its anchor is named, and nothing is
/// written: this is the contract that makes anchors worth keeping.
#[test]
fn a_deleted_anchor_stops_the_run_and_names_itself() {
    let app = copy_starter("anchors");

    let output = scaffold(&app, &["Project", "Team", "name:text_field"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let form = app.join("frontend/src/components/ProjectForm.tsx");
    let customized = read(&form).replace("    {/* 🐺 anubis:form-fields */}\n", "");
    std::fs::write(&form, &customized).expect("the copied form is writable");

    let output = scaffold_field(&app, &["Project", "priority:text_field"]);
    assert!(!output.status.success(), "a missing anchor must refuse");
    let message = stderr(&output);
    assert!(
        message.contains("frontend/src/components/ProjectForm.tsx"),
        "the message must name the file: {message}",
    );
    assert!(
        message.contains("🐺 anubis:form-fields"),
        "the message must name the anchor: {message}",
    );
    assert!(message.contains("restore the anchor"), "{message}");

    // The run is planned before it writes, so nothing landed anywhere.
    assert!(
        !read(&app.join("backend/src/schema.rs")).contains("priority ->"),
        "a refused run must not touch the schema",
    );
    assert!(
        !read(&app.join("backend/src/projects/model.rs")).contains("priority"),
        "a refused run must not touch the model",
    );
    assert!(
        std::fs::read_dir(app.join("backend/migrations"))
            .expect("the migrations directory is readable")
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains("priority")),
        "a refused run must not write a migration",
    );

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// Every field type reaches the artifacts a `scaffold model` run stamps, with
/// no leftover manual work, which is the gap this generator closed.
#[test]
fn every_field_type_is_wired_by_scaffold_model() {
    let app = copy_starter("field-types");

    let output = scaffold(
        &app,
        &[
            "Ticket",
            "Team",
            "urgency:number_field",
            "archived:boolean",
            "due_date:date_field",
        ],
    );
    assert!(
        output.status.success(),
        "scaffolding Ticket failed: {}",
        stderr(&output),
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("TODO"),
        "the run must promise no manual work",
    );

    // A boolean is the one type that is NOT NULL, because it has a default.
    let up = read(&migration(&app, "_create_tickets").join("up.sql"));
    assert!(up.contains("urgency INTEGER,"), "{up}");
    assert!(up.contains("archived BOOLEAN NOT NULL DEFAULT false,"));
    assert!(up.contains("due_date DATE,"));

    let model = read(&app.join("backend/src/tickets/model.rs"));
    assert!(model.contains("pub urgency: Option<i32>,"), "{model}");
    assert!(model.contains("pub archived: bool,"));
    assert!(model.contains("pub due_date: Option<chrono::NaiveDate>,"));
    assert!(model.contains("pub archived: Option<bool>,"));

    let routes = read(&app.join("backend/src/tickets/routes.rs"));
    assert!(
        routes.contains("archived: body.archived.unwrap_or(false),"),
        "{routes}",
    );
    assert!(routes.contains("let urgency = body.urgency.map(Some);"));

    let form = read(&app.join("frontend/src/components/TicketForm.tsx"));
    assert!(form.contains("NumberField,"), "{form}");
    assert!(form.contains("BooleanField,"));
    assert!(form.contains("DateField,"));
    assert!(form.contains("urgency: z.number().nullable(),"));
    assert!(form.contains("archived: z.boolean(),"));
    assert!(form.contains("due_date: z.string().nullable(),"));
    assert!(!form.contains("TODO"), "no manual work is left behind");

    let api = read(&app.join("frontend/src/api/routes/ticketRoutes.ts"));
    assert!(api.contains("  urgency: number | null\n"), "{api}");
    assert!(api.contains("  archived: boolean\n"));
    assert!(api.contains("archived?: boolean"));

    let locale = read(&app.join("frontend/src/locales/models/tickets.en-US.json"));
    assert!(locale.contains("\"dueDate\": \"Due date\""), "{locale}");
    // A column may share a name with the model's own strings, which is why
    // field strings live in their own object.
    assert_eq!(locale.matches("\"title\":").count(), 1);
    serde_json::from_str::<serde_json::Value>(&locale).expect("the locale file stays valid JSON");

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// A join and the association that reads through it, end to end.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one join and its association inspected artifact by artifact"
)]
fn a_join_carries_an_association_between_two_team_owned_models() {
    let app = copy_starter("join");

    for model in ["Project", "Tag"] {
        let output = scaffold(&app, &[model, "Team", "name:text_field"]);
        assert!(
            output.status.success(),
            "scaffolding {model} failed: {}",
            stderr(&output),
        );
    }

    let output = scaffold_join(
        &app,
        &[
            "AppliedTag",
            "project_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        ],
    );
    assert!(
        output.status.success(),
        "scaffolding the join failed: {}",
        stderr(&output),
    );

    for expected in [
        "backend/src/applied_tags/mod.rs",
        "backend/src/applied_tags/model.rs",
        "backend/src/applied_tags/routes.rs",
        "backend/tests/applied_tags_flow.rs",
        "frontend/src/api/routes/appliedTagRoutes.ts",
    ] {
        assert!(app.join(expected).is_file(), "missing {expected}");
    }
    // A join is infrastructure, not a resource: it takes no permissions.
    let roles = read(&app.join("config/roles.yml"));
    assert!(!roles.contains("AppliedTag"), "{roles}");

    // The migration carries both cascades, the pair's uniqueness, and the trigger.
    let up = read(&migration(&app, "_create_applied_tags").join("up.sql"));
    assert!(
        up.contains("project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE"),
        "{up}",
    );
    assert!(up.contains("tag_id UUID NOT NULL REFERENCES tags(id) ON DELETE CASCADE"));
    assert!(up.contains("UNIQUE (project_id, tag_id)"));
    assert!(up.contains("CREATE TRIGGER set_updated_at BEFORE UPDATE ON applied_tags"));

    // The schema joins both ways, and both pairs may share a query.
    let schema = read(&app.join("backend/src/schema.rs"));
    assert!(schema.contains("    applied_tags (id) {"), "{schema}");
    assert!(schema.contains("diesel::joinable!(applied_tags -> projects (project_id));"));
    assert!(schema.contains("diesel::joinable!(applied_tags -> tags (tag_id));"));
    assert!(
        schema.contains("diesel::allow_tables_to_appear_in_same_query!(projects, applied_tags);")
    );
    assert!(schema.contains("diesel::allow_tables_to_appear_in_same_query!(tags, applied_tags);"));

    // The join declares itself, so the field scaffolder can find it by pair.
    let model = read(&app.join("backend/src/applied_tags/model.rs"));
    assert!(
        model.contains("pub const JOIN: [&str; 3] = [\"AppliedTag\", \"Project\", \"Tag\"];"),
        "{model}",
    );
    assert!(model.contains("pub async fn valid_tags("));
    assert!(model.contains("pub async fn tag_ids_by_project("));
    for token in TEMPLATE_TOKENS.into_iter().chain(JOIN_TEMPLATE_TOKENS) {
        assert!(
            !model.contains(token),
            "the join model still contains `{token}`"
        );
    }

    // The association itself lands on the owning side.
    let output = scaffold_field(&app, &["Project", "tag_ids:super_select{class_name=Tag}"]);
    assert!(
        output.status.success(),
        "scaffolding the association failed: {}",
        stderr(&output),
    );
    // It declares no column, so it writes no migration and no schema entry.
    assert!(
        std::fs::read_dir(app.join("backend/migrations"))
            .expect("the migrations directory is readable")
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains("tag_ids")),
        "an association writes no migration",
    );
    assert!(
        !read(&app.join("backend/src/schema.rs")).contains("tag_ids ->"),
        "an association adds no column",
    );

    let routes = read(&app.join("backend/src/projects/routes.rs"));
    assert_eq!(
        routes.matches("tag_ids: Option<Vec<Uuid>>,").count(),
        2,
        "both request bodies carry the association: {routes}",
    );
    assert_eq!(
        routes
            .matches("crate::applied_tags::AppliedTag::replace_all(")
            .count(),
        2,
        "create and update both reconcile: {routes}",
    );
    assert!(routes.contains("AppliedTag::tag_ids_by_project("));
    assert!(routes.contains("    tag_ids: Vec<Uuid>,"), "{routes}");

    let api = read(&app.join("frontend/src/api/routes/projectRoutes.ts"));
    assert!(api.contains("  tag_ids: string[]\n"), "{api}");
    assert_eq!(api.matches("tag_ids?: string[]").count(), 2);

    let form = read(&app.join("frontend/src/components/ProjectForm.tsx"));
    assert!(form.contains("SuperSelectField,"), "{form}");
    assert!(form.contains("import { useAppliedTagOptions } from '../api/routes/appliedTagRoutes'"));
    assert!(form.contains("const tagOptions = useAppliedTagOptions(props.teamId)"));
    assert!(form.contains("options={tagOptions}"));
    assert!(form.contains("tag_ids: editing?.tag_ids ?? [],"));

    let locale = read(&app.join("frontend/src/locales/models/projects.en-US.json"));
    assert!(locale.contains("\"tagIds\": \"Tags\""), "{locale}");
    serde_json::from_str::<serde_json::Value>(&locale).expect("the locale file stays valid JSON");

    // The same association twice refuses, and a pair is joined only once.
    let again = scaffold_field(&app, &["Project", "tag_ids:super_select{class_name=Tag}"]);
    assert!(
        !again.status.success(),
        "a repeated association must refuse"
    );
    assert!(
        stderr(&again).contains("already carries the `tag_ids` association"),
        "unexpected error: {}",
        stderr(&again),
    );
    let twice = scaffold_join(
        &app,
        &[
            "TaggedProject",
            "project_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        ],
    );
    assert!(
        !twice.status.success(),
        "a second join for a pair must refuse"
    );
    assert!(
        stderr(&twice).contains("`AppliedTag` already links"),
        "unexpected error: {}",
        stderr(&twice),
    );

    std::fs::remove_dir_all(&app).expect("scratch directories are removable");
}

/// A join needs two team-owned models, and an association needs a join.
#[test]
fn join_and_association_arguments_are_rejected_with_a_reason() {
    let app = copy_starter("join-rejections");

    let output = scaffold(&app, &["Project", "Team", "name:text_field"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let output = scaffold(&app, &["Goal", "Project,Team", "name:text_field"]);
    assert!(output.status.success(), "{}", stderr(&output));

    // A side that does not exist names the command that would create it.
    let missing = scaffold_join(
        &app,
        &[
            "AppliedTag",
            "project_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        ],
    );
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("anubis scaffold model Tag Team"),
        "unexpected error: {}",
        stderr(&missing),
    );

    // A side owned through a parent is refused, pointing at the roadmap.
    let nested = scaffold_join(
        &app,
        &[
            "AppliedGoal",
            "project_id{class_name=Project}",
            "goal_id{class_name=Goal}",
        ],
    );
    assert!(!nested.status.success());
    assert!(
        stderr(&nested).contains("not owned directly by a team"),
        "unexpected error: {}",
        stderr(&nested),
    );

    // An association with no join names the join command that would back it.
    let unjoined = scaffold_field(&app, &["Project", "goal_ids:super_select{class_name=Goal}"]);
    assert!(!unjoined.status.success());
    assert!(
        stderr(&unjoined)
            .contains("anubis scaffold join <JoinModel> project_id{class_name=Project}"),
        "unexpected error: {}",
        stderr(&unjoined),
    );

    // And `scaffold model` refuses one outright: the join cannot exist yet.
    let at_model_time = scaffold(
        &app,
        &["Ticket", "Team", "tag_ids:super_select{class_name=Tag}"],
    );
    assert!(!at_model_time.status.success());
    assert!(
        stderr(&at_model_time).contains("anubis scaffold join"),
        "unexpected error: {}",
        stderr(&at_model_time),
    );

    assert!(!app.join("backend/src/applied_tags").exists());
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

/// Runs `anubis scaffold join` inside `app`.
fn scaffold_join(app: &Path, arguments: &[&str]) -> Output {
    Command::new(ANUBIS)
        .args(["scaffold", "join"])
        .args(arguments)
        .current_dir(app)
        .output()
        .expect("the anubis binary runs")
}

/// Runs `anubis scaffold field` inside `app`.
fn scaffold_field(app: &Path, arguments: &[&str]) -> Output {
    Command::new(ANUBIS)
        .args(["scaffold", "field"])
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
