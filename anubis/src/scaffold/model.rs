//! Planning one `anubis scaffold model` run.
//!
//! [`ModelScaffold`] turns the command line arguments into every decision the
//! generator has to make: which living template to transform, how template
//! names map onto the target model's names, what the new module, table, and
//! migration are called, and which lines the shared application files receive
//! above their anchors. It performs no I/O, so the whole plan is testable as
//! plain strings and the CLI stays a thin filesystem shell.
//!
//! ```
//! use anubis::scaffold::ModelScaffold;
//!
//! let scaffold = ModelScaffold::parse("Project", "Team", &["name:text_field".to_owned()])?;
//! assert_eq!(scaffold.module(), "projects");
//! assert_eq!(scaffold.module_declaration(), "pub mod projects;");
//! assert_eq!(scaffold.template().module(), "absolutely_abstract");
//! # Ok::<(), anubis::scaffold::ScaffoldError>(())
//! ```

use super::error::ScaffoldError;
use super::field::{Artifact, Field, FieldScaffold};
use super::inflect::Names;
use super::stamp::Replacements;

/// A living template in the application, one per ownership depth.
///
/// The templates live in `backend/src/scaffolding/`, are compiled and
/// integration-tested with the rest of the application, and are read from
/// disk at generation time. The generator only needs to know their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTemplate {
    /// The ownership chain, from the template's own model outward to the team.
    /// Each link pairs a model name with the module under
    /// `backend/src/scaffolding/` that holds its slice, and the first link is
    /// the template itself. `Team` is not a link: it belongs to the framework
    /// and no template stamps it.
    chain: &'static [(&'static str, &'static str)],
    frontend: &'static [&'static str],
}

/// The template for a model owned directly by a team.
const TEAM_OWNED: ModelTemplate = ModelTemplate {
    chain: &[("CreativeConcept", "absolutely_abstract")],
    frontend: &[
        "frontend/src/api/routes/creativeConceptRoutes.ts",
        "frontend/src/components/CreativeConceptForm.tsx",
        "frontend/src/locales/models/creativeConcepts.en-US.json",
        "frontend/src/pages/CreativeConceptPage.tsx",
        "frontend/src/pages/CreativeConceptsPage.tsx",
    ],
};

/// The template for a model owned through a team-owned parent.
///
/// A nested model has no list page: its table is the section component its
/// parent's show page renders. It does own a show page, because that page is
/// what the next depth down attaches its own section to.
const NESTED: ModelTemplate = ModelTemplate {
    chain: &[
        ("TangibleThing", "completely_concrete"),
        ("CreativeConcept", "absolutely_abstract"),
    ],
    frontend: &[
        "frontend/src/api/routes/tangibleThingRoutes.ts",
        "frontend/src/components/TangibleThingForm.tsx",
        "frontend/src/components/TangibleThingsSection.tsx",
        "frontend/src/locales/models/tangibleThings.en-US.json",
        "frontend/src/pages/TangibleThingPage.tsx",
    ],
};

/// The template for a model owned through a nested parent.
///
/// The same shape as [`NESTED`] one level further out: the chain resolves
/// through two joins rather than one, and the team every handler needs comes
/// off the chain's root, which is the only link that carries a `team_id`.
const DEEPLY_NESTED: ModelTemplate = ModelTemplate {
    chain: &[
        ("GranularDetail", "exceedingly_granular"),
        ("TangibleThing", "completely_concrete"),
        ("CreativeConcept", "absolutely_abstract"),
    ],
    frontend: &[
        "frontend/src/api/routes/granularDetailRoutes.ts",
        "frontend/src/components/GranularDetailForm.tsx",
        "frontend/src/components/GranularDetailsSection.tsx",
        "frontend/src/locales/models/granularDetails.en-US.json",
        "frontend/src/pages/GranularDetailPage.tsx",
    ],
};

/// How many parents an ownership chain may name before the team.
///
/// Two, because three living templates prove three depths, and a template is
/// the only thing that can teach the generator a join it has never written.
const MAX_ANCESTORS: usize = 2;

impl ModelTemplate {
    /// The template model's name, e.g. `TangibleThing`.
    #[must_use]
    pub fn model(self) -> &'static str {
        self.chain[0].0
    }

    /// The template's ancestors, nearest parent first, as name and module.
    #[must_use]
    pub fn ancestors(self) -> &'static [(&'static str, &'static str)] {
        &self.chain[1..]
    }

    /// The template parent's name, for a nested template.
    #[must_use]
    pub fn parent(self) -> Option<&'static str> {
        self.ancestors().first().map(|(model, _module)| *model)
    }

    /// The template module under `backend/src/scaffolding/`.
    #[must_use]
    pub fn module(self) -> &'static str {
        self.chain[0].1
    }

    /// The template's frontend files, relative to the application root.
    ///
    /// Each one transforms into the target model's file: the paths carry the
    /// template's name too, so the destination is the transformed path.
    #[must_use]
    pub fn frontend_files(self) -> &'static [&'static str] {
        self.frontend
    }

    /// The template model's table, e.g. `tangible_things`.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn table(self) -> String {
        Names::parse(self.model())
            .expect("template names are valid")
            .snake_plural()
    }

    /// The tables of the template's ancestors, nearest parent first.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn ancestor_tables(self) -> Vec<String> {
        self.ancestors()
            .iter()
            .map(|(model, _module)| {
                Names::parse(model)
                    .expect("template names are valid")
                    .snake_plural()
            })
            .collect()
    }
}

/// The columns every scaffolded model inherits from the living template.
///
/// The template proves them end to end, so a scaffold gets them for free:
/// `name` is the required display column and `description` the optional
/// long-form one. Naming either in the field list is the identity case;
/// naming it with the other type is a mistake worth reporting.
const TEMPLATE_FIELDS: [(&str, &str); 2] = [("name", "text_field"), ("description", "text_area")];

/// `rustfmt`'s default `max_width`. Generated statements that would exceed it
/// are emitted pre-wrapped, so `cargo fmt --check` passes on untouched output.
pub(super) const MAX_WIDTH: usize = 100;

/// The `max-len` the frontend's `ESLint` configuration enforces.
const TS_MAX_WIDTH: usize = 120;

/// The indentation the `account_router` body sits at.
pub(super) const ROUTER_INDENT: usize = 4;

/// One planned `anubis scaffold model <Model> <ParentChain> [field:type ...]`.
///
/// # Examples
/// ```
/// use anubis::scaffold::ModelScaffold;
///
/// let nested = ModelScaffold::parse("Goal", "Project,Team", &[])?;
/// assert_eq!(nested.table(), "goals");
/// assert_eq!(nested.parent().map(anubis::scaffold::Names::pascal), Some("Project".to_owned()));
/// assert_eq!(
///     nested.replacements().apply("mod tangible_things; struct TangibleThing;"),
///     "mod goals; struct Goal;",
/// );
/// # Ok::<(), anubis::scaffold::ScaffoldError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ModelScaffold {
    model: Names,
    /// The chain above the model, nearest parent first, `Team` excluded.
    /// Empty for a team-owned model.
    ancestors: Vec<Names>,
    fields: Vec<Field>,
}

impl ModelScaffold {
    /// Plans a scaffold from the command line arguments.
    ///
    /// `ownership` is the comma-separated chain ending in `Team`, exactly as
    /// Bullet Train writes it: `Team` for a team-owned model, `Project,Team`
    /// for a model owned through `Project`, `Goal,Project,Team` for one owned
    /// through a nested parent.
    ///
    /// # Errors
    /// Returns an error when a name does not parse, when the chain does not
    /// end in `Team`, when it names more than two parents or the same model
    /// twice, or when a field argument is malformed, duplicated, or names a
    /// column the scaffolder maintains.
    pub fn parse(model: &str, ownership: &str, fields: &[String]) -> Result<Self, ScaffoldError> {
        let model = Names::parse(model)?;
        let ancestors = parse_ownership(ownership, &model)?;

        let fields = fields
            .iter()
            .map(|argument| Field::parse(argument))
            .collect::<Result<Vec<_>, _>>()?;
        validate_fields(&fields, ancestors.first())?;

        Ok(Self {
            model,
            ancestors,
            fields,
        })
    }

    /// The model's names.
    #[must_use]
    pub fn model(&self) -> &Names {
        &self.model
    }

    /// The parent model's names, for a nested model.
    #[must_use]
    pub fn parent(&self) -> Option<&Names> {
        self.ancestors.first()
    }

    /// The model's whole ownership chain, nearest parent first.
    ///
    /// Empty for a team-owned model, one entry for a nested one, two for a
    /// grandchild. The length is what selects the living template.
    #[must_use]
    pub fn ancestors(&self) -> &[Names] {
        &self.ancestors
    }

    /// The living template this scaffold transforms.
    #[must_use]
    pub fn template(&self) -> ModelTemplate {
        match self.ancestors.len() {
            0 => TEAM_OWNED,
            1 => NESTED,
            // `parse` is the only constructor, and it refuses a deeper chain.
            _two => DEEPLY_NESTED,
        }
    }

    /// The full rewrite from template names to this model's names.
    ///
    /// A nested model rewrites its own name and every ancestor's, plus the
    /// template module paths: the generated code refers to the modules the
    /// earlier scaffolds created, never to the templates they came from.
    #[must_use]
    pub fn replacements(&self) -> Replacements {
        let template = self.template();
        let mut pairs = name_pairs(template.model(), &self.model);
        pairs.extend(module_pairs(template.module(), &self.module()));

        for ((template_name, template_module), ancestor) in
            template.ancestors().iter().zip(&self.ancestors)
        {
            pairs.extend(name_pairs(template_name, ancestor));
            pairs.extend(module_pairs(template_module, &ancestor.snake_plural()));
        }

        Replacements::new(pairs)
    }

    /// The application module the model's slice lives in, e.g. `projects`.
    #[must_use]
    pub fn module(&self) -> String {
        self.model.snake_plural()
    }

    /// The database table, e.g. `projects`.
    #[must_use]
    pub fn table(&self) -> String {
        self.model.snake_plural()
    }

    /// The migration directory name for `version`, e.g.
    /// `2026-08-15-101112_create_projects`.
    #[must_use]
    pub fn migration_directory(&self, version: &str) -> String {
        format!("{version}_create_{}", self.table())
    }

    /// The module declaration inserted into `backend/src/lib.rs`.
    ///
    /// Model modules are public: an application's own library is what its
    /// binary, its tests, and any sibling crate build on, and a re-exported
    /// model type that nothing outside the module can name is dead weight.
    #[must_use]
    pub fn module_declaration(&self) -> String {
        format!("pub mod {};", self.module())
    }

    /// The router mount inserted into `account_router`.
    ///
    /// Emitted on one line when it fits `rustfmt`'s width, and pre-wrapped the
    /// way `rustfmt` would wrap it when it does not, so generated code is
    /// format-clean without running the formatter.
    #[must_use]
    pub fn route_mount(&self) -> String {
        let module = self.module();
        let single =
            format!("router = router.merge({module}::router(pool.clone(), roles.clone()));");
        if single.len() + ROUTER_INDENT <= MAX_WIDTH {
            single
        } else {
            format!(
                "router = router.merge({module}::router(\n    pool.clone(),\n    roles.clone(),\n));"
            )
        }
    }

    /// The `/api/v1` router mount inserted into `api_v1_router`.
    ///
    /// Wrapped the way [`route_mount`] is, and for the same reason.
    ///
    /// [`route_mount`]: ModelScaffold::route_mount
    #[must_use]
    pub fn api_route_mount(&self) -> String {
        let module = self.module();
        let single =
            format!("router = router.merge({module}::api_router(pool.clone(), roles.clone()));");
        if single.len() + ROUTER_INDENT <= MAX_WIDTH {
            single
        } else {
            format!(
                "router = router.merge({module}::api_router(\n    pool.clone(),\n    roles.clone(),\n));"
            )
        }
    }

    /// The OpenAPI merge inserted into the application's `openapi` function.
    ///
    /// The model's own `routes.rs` carries the `utoipa` registrations for its
    /// paths and schemas, so the application's document gains a whole model in
    /// one line.
    #[must_use]
    pub fn api_doc_merge(&self) -> String {
        format!("document.merge({}::openapi());", self.module())
    }

    /// The `config/roles.yml` grant line for a role, e.g. `Project: [manage]`.
    #[must_use]
    pub fn role_grant(&self, actions: &str) -> String {
        format!("{}: [{actions}]", self.model.pascal())
    }

    /// The requested fields the living template does not already carry.
    ///
    /// The template supplies `name` and `description`; every other field is
    /// planned exactly as `anubis scaffold field` would plan it, and reaches
    /// the same artifacts through the same insertions. That shared plan is
    /// what makes a field declared at scaffold time and a field added a month
    /// later land identically.
    #[must_use]
    pub fn added_fields(&self) -> Vec<FieldScaffold> {
        self.fields
            .iter()
            .filter(|field| {
                !TEMPLATE_FIELDS
                    .iter()
                    .any(|(name, _type)| *name == field.name())
            })
            .map(|field| FieldScaffold::new(self.model.clone(), field.clone()))
            .collect()
    }

    /// The `CREATE TABLE` lines for the fields the template does not carry.
    #[must_use]
    pub fn added_sql_columns(&self) -> Vec<String> {
        self.added_fields()
            .iter()
            .filter_map(|field| field.field().sql_column())
            .map(|column| format!("{column},"))
            .collect()
    }

    /// The `diesel::table!` lines for the fields the template does not carry.
    #[must_use]
    pub fn added_schema_columns(&self) -> Vec<String> {
        self.added_fields()
            .iter()
            .filter_map(|field| field.field().schema_column())
            .collect()
    }
}

/// The frontend slice: the lines shared application files receive, and the
/// names the generated components carry.
impl ModelScaffold {
    /// The `UrlTree` entries inserted into `frontend/src/urls.ts`.
    ///
    /// Every model owns a show page, and so a route to it. Only a team-owned
    /// model owns a list page: a nested model's table lives on its parent's
    /// show page, which is what a grandchild attaches its own table to.
    #[must_use]
    pub fn url_entries(&self) -> String {
        let show = self.model.camel();
        let path = self.model.kebab_plural();
        let member = format!("{show}: '/{path}/:{show}Id',");
        if self.ancestors.is_empty() {
            format!("{}: '/{path}',\n{member}", self.model.camel_plural())
        } else {
            member
        }
    }

    /// The link factory inserted into `frontend/src/urls.ts`.
    ///
    /// Emitted pre-wrapped when a long model name would push the signature or
    /// the body past the frontend's `max-len`, the way [`route_mount`] handles
    /// `rustfmt`'s width, so the output lints clean untouched.
    ///
    /// [`route_mount`]: ModelScaffold::route_mount
    #[must_use]
    pub fn url_factory(&self) -> String {
        let show = self.model.camel();
        let parameter = format!("{show}Id");
        let function = format!("get{}Url", self.model.pascal());

        let signature = format!("export function {function}({parameter}: string): string {{");
        let signature = if signature.len() <= TS_MAX_WIDTH {
            signature
        } else {
            format!("export function {function}(\n  {parameter}: string,\n): string {{")
        };

        let body = format!("  return UrlTree.{show}.replace(':{parameter}', {parameter})");
        let body = if body.len() <= TS_MAX_WIDTH {
            body
        } else {
            format!("  return UrlTree.{show}\n    .replace(':{parameter}', {parameter})")
        };

        format!("{signature}\n{body}\n}}\n\n")
    }

    /// The page imports inserted into `frontend/src/App.tsx`.
    ///
    /// One per page the model owns, which [`url_entries`] decides.
    ///
    /// [`url_entries`]: ModelScaffold::url_entries
    #[must_use]
    pub fn page_imports(&self) -> String {
        let show = self.show_page();
        let show_import = format!("import {{ {show} }} from './pages/{show}'");
        if self.ancestors.is_empty() {
            let list = self.list_page();
            format!("{show_import}\nimport {{ {list} }} from './pages/{list}'")
        } else {
            show_import
        }
    }

    /// The `<Route>` elements inserted into `frontend/src/App.tsx`.
    #[must_use]
    pub fn route_elements(&self) -> String {
        let show = route_element(&self.model.camel(), &self.show_page());
        if self.ancestors.is_empty() {
            let list = route_element(&self.model.camel_plural(), &self.list_page());
            format!("{list}\n{show}")
        } else {
            show
        }
    }

    /// The navigation entry inserted into `AppShell.tsx`.
    #[must_use]
    pub fn nav_item(&self) -> String {
        let list = self.model.camel_plural();
        format!(
            "<NavbarItem>
  <Link to={{UrlTree.{list}}} className='opacity-80 hover:opacity-100'>{{
      t('{list}.navLink')
    }}</Link>
</NavbarItem>",
        )
    }

    /// The locale import inserted into `frontend/src/i18n.ts`.
    #[must_use]
    pub fn locale_import(&self) -> String {
        let models = self.model.camel_plural();
        format!("import {models}EnUS from './locales/models/{models}.en-US.json'")
    }

    /// The locale spread inserted into `frontend/src/i18n.ts`.
    #[must_use]
    pub fn locale_spread(&self) -> String {
        format!("...{}EnUS,", self.model.camel_plural())
    }

    /// How a nested model attaches to the page its parent's scaffold wrote.
    ///
    /// `None` for a team-owned model, which is nobody's child. Every other
    /// depth attaches identically, because every parent owns a show page.
    #[must_use]
    pub fn child_attachment(&self) -> Option<ChildAttachment> {
        let parent = self.ancestors.first()?;
        let section = format!("{}Section", self.model.pascal_plural());
        let parent_id = format!("{}Id", parent.camel());
        Some(ChildAttachment {
            page: format!("frontend/src/pages/{}Page.tsx", parent.pascal()),
            import: format!("import {{ {section} }} from '../components/{section}'"),
            // The parent page reads its own id out of the route, under the
            // name the parent's own scaffold gave it, and hands down the team
            // a scaffolded association's options are scoped to. Wrapped the
            // way a long model name would need, exactly as the link factory is.
            element: section_element(&section, &parent_id),
        })
    }

    /// The list page component, e.g. `ProjectsPage`.
    fn list_page(&self) -> String {
        format!("{}Page", self.model.pascal_plural())
    }

    /// The show page component, e.g. `ProjectPage`.
    fn show_page(&self) -> String {
        format!("{}Page", self.model.pascal())
    }
}

/// Every artifact a scaffolded model owns, and what each one receives.
///
/// The paths are relative to the application root, and every ownership depth's
/// files are listed: a team-owned model owns a list page, a nested model owns
/// the section component its parent renders instead, and both own a show page.
/// A caller applies the entries whose file exists, which is how `anubis
/// scaffold field` works at any depth without being told which it is looking at.
///
/// The model's locale file is not here: it is JSON, so it takes structural
/// insertion rather than anchors. [`locale_file`] names it.
///
/// # Examples
/// ```
/// use anubis::scaffold::{Artifact, Names, model_artifacts};
///
/// let files = model_artifacts(&Names::parse("Project")?);
/// assert!(files.contains(&("backend/src/projects/model.rs".to_owned(), Artifact::Model)));
/// assert!(files.contains(&(
///     "frontend/src/components/ProjectForm.tsx".to_owned(),
///     Artifact::Form,
/// )));
/// # Ok::<(), anubis::scaffold::ScaffoldError>(())
/// ```
#[must_use]
pub fn model_artifacts(model: &Names) -> Vec<(String, Artifact)> {
    let module = model.snake_plural();
    let pascal = model.pascal();
    let pascal_plural = model.pascal_plural();
    vec![
        (format!("backend/src/{module}/model.rs"), Artifact::Model),
        (format!("backend/src/{module}/routes.rs"), Artifact::Routes),
        (format!("backend/tests/{module}_flow.rs"), Artifact::Test),
        (
            format!("frontend/src/api/routes/{}Routes.ts", model.camel()),
            Artifact::ApiRoutes,
        ),
        (
            format!("frontend/src/components/{pascal}Form.tsx"),
            Artifact::Form,
        ),
        (
            format!("frontend/src/pages/{pascal_plural}Page.tsx"),
            Artifact::Table,
        ),
        (
            format!("frontend/src/components/{pascal_plural}Section.tsx"),
            Artifact::Table,
        ),
        (
            format!("frontend/src/pages/{pascal}Page.tsx"),
            Artifact::ShowPage,
        ),
    ]
}

/// The model's locale file, relative to the application root.
#[must_use]
pub fn locale_file(model: &Names) -> String {
    format!(
        "frontend/src/locales/models/{}.en-US.json",
        model.camel_plural(),
    )
}

/// The page a nested model attaches to, and the two lines it inserts there.
///
/// Both anchors live in every show page a scaffold writes, so a child can
/// attach to a parent generated long before it.
#[derive(Debug, Clone)]
pub struct ChildAttachment {
    /// The parent's show page, relative to the application root.
    pub page: String,
    /// The import inserted above `🐺 anubis:child-imports`.
    pub import: String,
    /// The section element inserted above `🐺 anubis:children`.
    pub element: String,
}

/// The section element a nested model's scaffold inserts into its parent page.
///
/// Emitted on one line when it fits the frontend's `max-len`, and split the way
/// a reviewer would split it when a long model name pushes it past.
fn section_element(section: &str, parent_id: &str) -> String {
    // The page reads its own indentation onto every inserted line, so the
    // width has to allow for it.
    const PAGE_INDENT: usize = 4;

    let single = format!("<{section} {parent_id}={{{parent_id}}} teamId={{teamId}} />");
    if single.len() + PAGE_INDENT <= TS_MAX_WIDTH {
        single
    } else {
        format!("<{section}\n  {parent_id}={{{parent_id}}}\n  teamId={{teamId}}\n/>")
    }
}

/// One `<Route>` element, wrapped in the workspace gate like every app page.
fn route_element(path: &str, page: &str) -> String {
    format!(
        "<Route
  path={{UrlTree.{path}}}
  element={{
    <Workspace>
      <{page} />
    </Workspace>
  }}
/>",
    )
}

/// Parses the ownership chain into the model's ancestors, nearest first.
fn parse_ownership(ownership: &str, model: &Names) -> Result<Vec<Names>, ScaffoldError> {
    let chain = ownership
        .split(',')
        .map(str::trim)
        .filter(|link| !link.is_empty())
        .collect::<Vec<_>>();

    let Some((owner, parents)) = chain.split_last() else {
        return Err(ScaffoldError::new(
            "the ownership chain is empty; pass `Team` for a team-owned model".to_owned(),
        ));
    };
    if Names::parse(owner)?.pascal() != "Team" {
        return Err(ScaffoldError::new(format!(
            "the ownership chain must end in `Team`, not `{owner}`: every application record \
             reaches a team"
        )));
    }
    if parents.len() > MAX_ANCESTORS {
        return Err(ScaffoldError::new(format!(
            "ownership chain `{ownership}` names {} parents; scaffolding supports `Team`, \
             `<Parent>,Team`, and `<Parent>,<GrandParent>,Team`, because a living template is \
             what proves each depth and there are three of them",
            parents.len(),
        )));
    }

    let mut ancestors: Vec<Names> = Vec::with_capacity(parents.len());
    for parent in parents {
        let parent = Names::parse(parent)?;
        if parent.pascal() == model.pascal() {
            return Err(ScaffoldError::new(format!(
                "`{}` cannot own itself",
                model.pascal()
            )));
        }
        // A repeated link would generate two modules under one name and a
        // chain that walks in a circle.
        if ancestors
            .iter()
            .any(|earlier| earlier.pascal() == parent.pascal())
        {
            return Err(ScaffoldError::new(format!(
                "`{}` appears twice in ownership chain `{ownership}`",
                parent.pascal(),
            )));
        }
        ancestors.push(parent);
    }
    Ok(ancestors)
}

/// Rejects duplicate fields, fields that fight the template, and fields that
/// collide with the column the ownership chain owns.
fn validate_fields(fields: &[Field], parent: Option<&Names>) -> Result<(), ScaffoldError> {
    let foreign_key = parent.map(|parent| format!("{}_id", parent.snake()));

    for (index, field) in fields.iter().enumerate() {
        if fields[..index]
            .iter()
            .any(|earlier| earlier.name() == field.name())
        {
            return Err(ScaffoldError::new(format!(
                "field `{}` is declared twice",
                field.name()
            )));
        }
        // An association points at something that has to exist already: a join
        // model for a has-many-through, a team-owned model for a belongs_to.
        // Bullet Train splits the commands for the same reason.
        if field.is_association() {
            return Err(ScaffoldError::new(format!(
                "field `{}` is an association, and an association reaches a model that already \
                 exists. Generate this model first, then add the field with \
                 `anubis scaffold field`.",
                field.name(),
            )));
        }
        if foreign_key.as_deref() == Some(field.name()) {
            return Err(ScaffoldError::new(format!(
                "field `{}` is the ownership chain's own column and is maintained by the \
                 scaffolder",
                field.name()
            )));
        }
        if let Some((_name, expected)) = TEMPLATE_FIELDS
            .iter()
            .find(|(name, _type)| *name == field.name())
            && field.type_name() != *expected
        {
            return Err(ScaffoldError::new(format!(
                "field `{}` comes from the living template as `{expected}` and cannot be \
                 declared as `{}`",
                field.name(),
                field.type_name(),
            )));
        }
    }
    Ok(())
}

/// The name-variant rewrites from a template name to a target name.
pub(super) fn name_pairs(template: &str, target: &Names) -> Vec<(String, String)> {
    let template = Names::parse(template).expect("template names are valid");
    Replacements::between(&template, target).into_pairs()
}

/// The module-path rewrites from a template module to an application module.
///
/// Both separators appear: `::` in `use` paths and router mounts, `/` in the
/// file paths the stamped module is written to.
pub(super) fn module_pairs(template: &str, target: &str) -> Vec<(String, String)> {
    vec![
        (format!("scaffolding::{template}"), target.to_owned()),
        (format!("scaffolding/{template}"), target.to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::ModelScaffold;

    fn fields(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_team_owned_model_transforms_the_parent_template() {
        let scaffold =
            ModelScaffold::parse("Project", "Team", &fields(&["name:text_field"])).unwrap();
        assert_eq!(scaffold.template().module(), "absolutely_abstract");
        assert!(scaffold.parent().is_none());
        assert_eq!(scaffold.module(), "projects");
        assert_eq!(scaffold.table(), "projects");
        assert_eq!(scaffold.module_declaration(), "pub mod projects;");
        assert_eq!(
            scaffold.route_mount(),
            "router = router.merge(projects::router(pool.clone(), roles.clone()));"
        );
        assert_eq!(
            scaffold.api_route_mount(),
            "router = router.merge(projects::api_router(pool.clone(), roles.clone()));"
        );
        assert_eq!(
            scaffold.api_doc_merge(),
            "document.merge(projects::openapi());"
        );
        assert_eq!(scaffold.role_grant("read"), "Project: [read]");
        assert_eq!(
            scaffold.migration_directory("2026-08-15-101112"),
            "2026-08-15-101112_create_projects"
        );
        assert!(scaffold.added_fields().is_empty());
    }

    #[test]
    fn the_replacements_rewrite_every_template_reference() {
        let scaffold = ModelScaffold::parse("Project", "Team", &[]).unwrap();
        let replacements = scaffold.replacements();
        assert_eq!(
            replacements.apply("use crate::schema::creative_concepts; struct CreativeConcept;"),
            "use crate::schema::projects; struct Project;",
        );
        assert_eq!(
            replacements.apply("crate::scaffolding::absolutely_abstract::CreativeConcept"),
            "crate::projects::Project",
        );
        assert_eq!(
            replacements.apply("src/scaffolding/absolutely_abstract/model.rs"),
            "src/projects/model.rs",
        );
        assert_eq!(
            replacements.apply("Name the creative concept."),
            "Name the project.",
        );
    }

    #[test]
    fn a_nested_model_rewrites_its_parent_too() {
        let scaffold = ModelScaffold::parse("Goal", "Project,Team", &[]).unwrap();
        assert_eq!(scaffold.template().module(), "completely_concrete");
        let replacements = scaffold.replacements();
        assert_eq!(
            replacements.apply("use crate::scaffolding::absolutely_abstract::CreativeConcept;"),
            "use crate::projects::Project;",
        );
        assert_eq!(
            replacements.apply("tangible_things::creative_concept_id"),
            "goals::project_id",
        );
        assert_eq!(
            replacements.apply("TangibleThing::valid_creative_concepts"),
            "Goal::valid_projects",
        );
    }

    #[test]
    fn a_grandchild_rewrites_its_whole_chain() {
        let scaffold = ModelScaffold::parse("Task", "Goal,Project,Team", &[]).unwrap();
        assert_eq!(scaffold.template().module(), "exceedingly_granular");
        assert_eq!(
            scaffold
                .ancestors()
                .iter()
                .map(super::Names::pascal)
                .collect::<Vec<_>>(),
            ["Goal", "Project"],
            "the chain reads from the model outward",
        );

        let replacements = scaffold.replacements();
        assert_eq!(
            replacements.apply("use crate::scaffolding::completely_concrete::TangibleThing;"),
            "use crate::goals::Goal;",
        );
        assert_eq!(
            replacements.apply("use crate::scaffolding::absolutely_abstract::CreativeConcept;"),
            "use crate::projects::Project;",
        );
        assert_eq!(
            replacements.apply("granular_details::tangible_thing_id"),
            "tasks::goal_id",
        );
        assert_eq!(
            replacements.apply("GranularDetail::valid_tangible_things"),
            "Task::valid_goals",
        );
        // The team every handler needs comes off the chain's root, which is
        // the only link carrying a `team_id`.
        assert_eq!(
            replacements.apply("creative_concept.team_id"),
            "project.team_id",
        );
    }

    #[test]
    fn a_grandchild_attaches_to_its_nested_parents_page() {
        let scaffold = ModelScaffold::parse("Task", "Goal,Project,Team", &[]).unwrap();

        let attachment = scaffold
            .child_attachment()
            .expect("a grandchild attaches to its parent");
        assert_eq!(attachment.page, "frontend/src/pages/GoalPage.tsx");
        assert_eq!(
            attachment.element, "<TasksSection goalId={goalId} teamId={teamId} />",
            "a section is handed its parent's id, whatever the parent's depth",
        );

        // A grandchild is listed on its parent's page, so it owns no list
        // page and no navigation entry, exactly as a nested model does.
        assert_eq!(scaffold.url_entries(), "task: '/tasks/:taskId',");
        assert_eq!(
            scaffold.page_imports(),
            "import { TaskPage } from './pages/TaskPage'",
        );
        assert!(scaffold.route_elements().contains("<TaskPage />"));
        assert!(!scaffold.route_elements().contains("<TasksPage />"));
    }

    #[test]
    fn added_fields_are_planned_like_a_scaffold_field_run() {
        let scaffold = ModelScaffold::parse(
            "Project",
            "Team",
            &fields(&["name:text_field", "summary:text_area", "archived:boolean"]),
        )
        .unwrap();

        let added = scaffold.added_fields();
        assert_eq!(added.len(), 2, "the template already carries `name`");
        assert_eq!(added[0].name(), "summary");
        assert_eq!(
            scaffold.added_sql_columns(),
            ["summary TEXT,", "archived BOOLEAN NOT NULL DEFAULT false,"],
        );
        assert_eq!(
            scaffold.added_schema_columns(),
            ["summary -> Nullable<Text>,", "archived -> Bool,"],
        );
        assert!(
            added[0]
                .insertions(super::Artifact::Form)
                .iter()
                .any(|(_anchor, line)| line.contains("t('projects.fields.summary')")),
            "an added field is planned against its own model's locale keys",
        );
    }

    #[test]
    fn a_models_artifacts_are_named_from_its_own_names() {
        let model = super::Names::parse("Project").unwrap();
        let files = super::model_artifacts(&model)
            .into_iter()
            .map(|(path, _artifact)| path)
            .collect::<Vec<_>>();
        assert!(files.contains(&"backend/src/projects/routes.rs".to_owned()));
        assert!(files.contains(&"backend/tests/projects_flow.rs".to_owned()));
        assert!(files.contains(&"frontend/src/api/routes/projectRoutes.ts".to_owned()));
        assert!(files.contains(&"frontend/src/pages/ProjectsPage.tsx".to_owned()));
        assert!(files.contains(&"frontend/src/pages/ProjectPage.tsx".to_owned()));
        assert!(files.contains(&"frontend/src/components/ProjectsSection.tsx".to_owned()));
        assert_eq!(
            super::locale_file(&model),
            "frontend/src/locales/models/projects.en-US.json",
        );
    }

    #[test]
    fn a_long_module_name_wraps_the_router_mount_the_way_rustfmt_would() {
        let scaffold =
            ModelScaffold::parse("InternationalDistributionAgreementAmendment", "Team", &[])
                .unwrap();
        let mount = scaffold.route_mount();
        assert!(mount.contains("::router(\n    pool.clone(),\n    roles.clone(),\n));"));
        assert!(
            mount.lines().all(|line| line.len() + 4 <= 100),
            "wrapped mount must fit rustfmt's width: {mount}",
        );

        let api_mount = scaffold.api_route_mount();
        assert!(api_mount.contains("::api_router(\n    pool.clone(),\n    roles.clone(),\n));"));
        assert!(
            api_mount.lines().all(|line| line.len() + 4 <= 100),
            "wrapped mount must fit rustfmt's width: {api_mount}",
        );
    }

    #[test]
    fn a_team_owned_model_wires_its_pages_into_the_shared_frontend_files() {
        let scaffold = ModelScaffold::parse("Project", "Team", &[]).unwrap();

        assert_eq!(
            scaffold.url_entries(),
            "projects: '/projects',\nproject: '/projects/:projectId',",
        );
        assert_eq!(
            scaffold.url_factory(),
            "export function getProjectUrl(projectId: string): string {\n  \
             return UrlTree.project.replace(':projectId', projectId)\n}\n\n",
        );
        assert_eq!(
            scaffold.page_imports(),
            "import { ProjectPage } from './pages/ProjectPage'\n\
             import { ProjectsPage } from './pages/ProjectsPage'",
        );
        assert!(
            scaffold
                .route_elements()
                .contains("path={UrlTree.projects}")
        );
        assert!(scaffold.route_elements().contains("<ProjectPage />"));
        assert!(scaffold.nav_item().contains("t('projects.navLink')"));
        assert_eq!(
            scaffold.locale_import(),
            "import projectsEnUS from './locales/models/projects.en-US.json'",
        );
        assert_eq!(scaffold.locale_spread(), "...projectsEnUS,");
        assert!(scaffold.child_attachment().is_none());
        assert!(
            scaffold
                .template()
                .frontend_files()
                .contains(&"frontend/src/pages/CreativeConceptsPage.tsx"),
        );
    }

    #[test]
    fn a_nested_model_attaches_to_its_parents_page() {
        let scaffold = ModelScaffold::parse("Goal", "Project,Team", &[]).unwrap();

        let attachment = scaffold
            .child_attachment()
            .expect("a nested model attaches to its parent");
        assert_eq!(attachment.page, "frontend/src/pages/ProjectPage.tsx");
        assert_eq!(
            attachment.import,
            "import { GoalsSection } from '../components/GoalsSection'",
        );
        assert_eq!(
            attachment.element, "<GoalsSection projectId={projectId} teamId={teamId} />",
            "a section is handed its parent's id and the team its options are scoped to",
        );
        // It owns a show page for its own children to attach to, and no list
        // page, because its table is the section above.
        assert_eq!(scaffold.url_entries(), "goal: '/goals/:goalId',");
        assert_eq!(
            scaffold.page_imports(),
            "import { GoalPage } from './pages/GoalPage'",
        );

        let replacements = scaffold.replacements();
        assert_eq!(
            replacements.apply("frontend/src/components/TangibleThingsSection.tsx"),
            "frontend/src/components/GoalsSection.tsx",
        );
        assert_eq!(
            replacements.apply("frontend/src/locales/models/tangibleThings.en-US.json"),
            "frontend/src/locales/models/goals.en-US.json",
        );
    }

    #[test]
    fn a_long_model_name_wraps_the_link_factory_the_way_eslint_wants() {
        let scaffold =
            ModelScaffold::parse("InternationalDistributionAgreementAmendment", "Team", &[])
                .unwrap();
        let factory = scaffold.url_factory();
        assert!(
            factory
                .contains("export function getInternationalDistributionAgreementAmendmentUrl(\n")
        );
        assert!(
            factory.lines().all(|line| line.len() <= 120),
            "the wrapped factory must fit the lint width: {factory}",
        );
    }

    #[test]
    fn ownership_chains_are_validated() {
        ModelScaffold::parse("Goal", "Project", &[]).unwrap_err();
        ModelScaffold::parse("Goal", "", &[]).unwrap_err();
        ModelScaffold::parse("Goal", "Goal,Team", &[]).unwrap_err();
        ModelScaffold::parse("Task", "Goal,Task,Team", &[]).unwrap_err();

        let repeated = ModelScaffold::parse("Task", "Goal,Goal,Team", &[]).unwrap_err();
        assert!(
            repeated.message().contains("appears twice"),
            "a repeated link must be named: {}",
            repeated.message(),
        );

        let deep = ModelScaffold::parse("Task", "Note,Goal,Project,Team", &[]).unwrap_err();
        assert!(
            deep.message().contains("<GrandParent>,Team"),
            "a fourth level must name the depths that exist: {}",
            deep.message(),
        );
    }

    #[test]
    fn fields_are_validated_against_the_template_and_the_chain() {
        ModelScaffold::parse("Project", "Team", &fields(&["name:text_area"])).unwrap_err();
        ModelScaffold::parse("Project", "Team", &fields(&["description:text_field"])).unwrap_err();
        ModelScaffold::parse(
            "Project",
            "Team",
            &fields(&["summary:text_area", "summary:text_area"]),
        )
        .unwrap_err();
        ModelScaffold::parse("Goal", "Project,Team", &fields(&["project_id:text_field"]))
            .unwrap_err();

        ModelScaffold::parse(
            "Project",
            "Team",
            &fields(&["name:text_field", "description:text_area"]),
        )
        .unwrap();
    }
}
