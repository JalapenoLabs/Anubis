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
use super::field::Field;
use super::inflect::Names;
use super::stamp::Replacements;

/// A living template in the application, one per ownership depth.
///
/// The templates live in `backend/src/scaffolding/`, are compiled and
/// integration-tested with the rest of the application, and are read from
/// disk at generation time. The generator only needs to know their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTemplate {
    model: &'static str,
    parent: Option<&'static str>,
    module: &'static str,
}

/// The template for a model owned directly by a team.
const TEAM_OWNED: ModelTemplate = ModelTemplate {
    model: "CreativeConcept",
    parent: None,
    module: "absolutely_abstract",
};

/// The template for a model owned through a team-owned parent.
const NESTED: ModelTemplate = ModelTemplate {
    model: "TangibleThing",
    parent: Some("CreativeConcept"),
    module: "completely_concrete",
};

impl ModelTemplate {
    /// The template model's name, e.g. `TangibleThing`.
    #[must_use]
    pub fn model(self) -> &'static str {
        self.model
    }

    /// The template parent's name, for the nested template.
    #[must_use]
    pub fn parent(self) -> Option<&'static str> {
        self.parent
    }

    /// The template module under `backend/src/scaffolding/`.
    #[must_use]
    pub fn module(self) -> &'static str {
        self.module
    }

    /// The template model's table, e.g. `tangible_things`.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn table(self) -> String {
        Names::parse(self.model)
            .expect("template names are valid")
            .snake_plural()
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
const MAX_WIDTH: usize = 100;

/// The indentation the `account_router` body sits at.
const ROUTER_INDENT: usize = 4;

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
    parent: Option<Names>,
    fields: Vec<Field>,
}

impl ModelScaffold {
    /// Plans a scaffold from the command line arguments.
    ///
    /// `ownership` is the comma-separated chain ending in `Team`, exactly as
    /// Bullet Train writes it: `Team` for a team-owned model, `Project,Team`
    /// for a model owned through `Project`.
    ///
    /// # Errors
    /// Returns an error when a name does not parse, when the chain does not
    /// end in `Team`, when the chain is deeper than one parent, or when a
    /// field argument is malformed, duplicated, or names a column the
    /// scaffolder maintains.
    pub fn parse(model: &str, ownership: &str, fields: &[String]) -> Result<Self, ScaffoldError> {
        let model = Names::parse(model)?;
        let parent = parse_ownership(ownership, &model)?;

        let fields = fields
            .iter()
            .map(|argument| Field::parse(argument))
            .collect::<Result<Vec<_>, _>>()?;
        validate_fields(&fields, parent.as_ref())?;

        Ok(Self {
            model,
            parent,
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
        self.parent.as_ref()
    }

    /// The living template this scaffold transforms.
    #[must_use]
    pub fn template(&self) -> ModelTemplate {
        if self.parent.is_some() {
            NESTED
        } else {
            TEAM_OWNED
        }
    }

    /// The full rewrite from template names to this model's names.
    ///
    /// A nested model rewrites two names, its own and its parent's, plus the
    /// template module paths: the generated code refers to the parent module
    /// the earlier scaffold created, never to the template it came from.
    #[must_use]
    pub fn replacements(&self) -> Replacements {
        let template = self.template();
        let mut pairs = name_pairs(template.model(), &self.model);
        pairs.extend(module_pairs(template.module(), &self.module()));

        if let (Some(template_parent), Some(parent)) = (template.parent(), self.parent.as_ref()) {
            pairs.extend(name_pairs(template_parent, parent));
            // The template's parent is the team-owned template, and the
            // generated code reaches the parent through the module its own
            // scaffold created.
            pairs.extend(module_pairs(TEAM_OWNED.module(), &parent.snake_plural()));
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

    /// The `config/roles.yml` grant line for a role, e.g. `Project: [manage]`.
    #[must_use]
    pub fn role_grant(&self, actions: &str) -> String {
        format!("{}: [{actions}]", self.model.pascal())
    }

    /// The requested fields the living template does not already carry.
    ///
    /// These reach the migration and the Diesel schema as nullable columns;
    /// wiring them through the model and the handlers is still manual, which
    /// is what [`ModelScaffold::manual_fields_note`] spells out.
    #[must_use]
    pub fn extra_fields(&self) -> Vec<&Field> {
        self.fields
            .iter()
            .filter(|field| {
                !TEMPLATE_FIELDS
                    .iter()
                    .any(|(name, _type)| *name == field.name())
            })
            .collect()
    }

    /// The `CREATE TABLE` lines for the fields the template does not carry.
    ///
    /// Every one of them is nullable whatever its field type says: nothing
    /// writes these columns until a developer wires them through the model,
    /// and a `NOT NULL` column with no writer fails every insert.
    #[must_use]
    pub fn extra_sql_columns(&self) -> Vec<String> {
        self.extra_fields()
            .iter()
            .map(|field| format!("{},", field.sql_column()))
            .collect()
    }

    /// The `diesel::table!` lines for the fields the template does not carry.
    ///
    /// Nullable for the same reason as [`ModelScaffold::extra_sql_columns`].
    #[must_use]
    pub fn extra_schema_columns(&self) -> Vec<String> {
        self.extra_fields()
            .iter()
            .map(|field| field.schema_column_as(true))
            .collect()
    }

    /// The comment planted at the top of a generated model that carries
    /// fields the generator could not wire all the way through.
    ///
    /// Returns `None` when every requested field comes from the template.
    #[must_use]
    pub fn manual_fields_note(&self) -> Option<String> {
        let extra = self.extra_fields();
        if extra.is_empty() {
            return None;
        }

        let model = self.model.pascal();
        let names = extra
            .iter()
            .map(|field| format!("`{}`", field.name()))
            .collect::<Vec<_>>()
            .join(", ");
        let (verb, columns, pronoun) = if extra.len() == 1 {
            ("lives", "a nullable column", "it")
        } else {
            ("live", "nullable columns", "them")
        };
        Some(format!(
            "// TODO(anubis): {names} {verb} in the migration and in\n\
             // backend/src/schema.rs as {columns}, and nowhere else. Add {pronoun} to\n\
             // {model}, New{model}, and {model}Changes below, then to the request bodies\n\
             // and the create and update handlers in routes.rs. `anubis scaffold field`\n\
             // will automate this.\n",
        ))
    }
}

/// Parses the ownership chain, returning the parent of a nested model.
fn parse_ownership(ownership: &str, model: &Names) -> Result<Option<Names>, ScaffoldError> {
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

    match parents {
        [] => Ok(None),
        [parent] => {
            let parent = Names::parse(parent)?;
            if parent.pascal() == model.pascal() {
                return Err(ScaffoldError::new(format!(
                    "`{}` cannot own itself",
                    model.pascal()
                )));
            }
            Ok(Some(parent))
        }
        _deeper => Err(ScaffoldError::new(format!(
            "ownership chain `{ownership}` nests deeper than one parent; scaffolding supports \
             `Team` and `<Parent>,Team` today, and deeper chains are on the roadmap"
        ))),
    }
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
            && field.field_type().name() != *expected
        {
            return Err(ScaffoldError::new(format!(
                "field `{}` comes from the living template as `{expected}` and cannot be \
                 declared as `{}`",
                field.name(),
                field.field_type().name(),
            )));
        }
    }
    Ok(())
}

/// The name-variant rewrites from a template name to a target name.
fn name_pairs(template: &str, target: &Names) -> Vec<(String, String)> {
    let template = Names::parse(template).expect("template names are valid");
    Replacements::between(&template, target).into_pairs()
}

/// The module-path rewrites from a template module to an application module.
///
/// Both separators appear: `::` in `use` paths and router mounts, `/` in the
/// file paths the stamped module is written to.
fn module_pairs(template: &str, target: &str) -> Vec<(String, String)> {
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
        assert_eq!(scaffold.role_grant("read"), "Project: [read]");
        assert_eq!(
            scaffold.migration_directory("2026-08-15-101112"),
            "2026-08-15-101112_create_projects"
        );
        assert!(scaffold.manual_fields_note().is_none());
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
    fn extra_fields_are_reported_as_manual_work() {
        let scaffold = ModelScaffold::parse(
            "Project",
            "Team",
            &fields(&["name:text_field", "summary:text_area"]),
        )
        .unwrap();
        let extra = scaffold.extra_fields();
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].name(), "summary");
        assert_eq!(scaffold.extra_sql_columns(), ["summary TEXT,"]);
        assert_eq!(
            scaffold.extra_schema_columns(),
            ["summary -> Nullable<Text>,"]
        );

        let note = scaffold.manual_fields_note().expect("a note is planted");
        assert!(note.contains("`summary`"));
        assert!(note.contains("a nullable column"), "note: {note}");
        assert!(note.contains("NewProject"));
        assert!(note.contains("ProjectChanges"));
        assert!(
            note.lines().all(|line| line.starts_with("// ")),
            "the note must be a Rust comment block: {note}",
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
    }

    #[test]
    fn ownership_chains_are_validated() {
        ModelScaffold::parse("Goal", "Project", &[]).unwrap_err();
        ModelScaffold::parse("Goal", "", &[]).unwrap_err();
        ModelScaffold::parse("Goal", "Goal,Team", &[]).unwrap_err();

        let deep = ModelScaffold::parse("Task", "Goal,Project,Team", &[]).unwrap_err();
        assert!(
            deep.message().contains("roadmap"),
            "deeper chains must point at the roadmap: {}",
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
