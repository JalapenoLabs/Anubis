//! Planning one `anubis scaffold join` run.
//!
//! A join model is the has-many-through half of Bullet Train's association
//! flow: `super_scaffold:join_model` there, `anubis scaffold join` here. It
//! connects two models that already exist and both reach the same team, and it
//! is infrastructure rather than a page, so what it generates is a table, a
//! model, the endpoints that attach and detach, and the proof that another
//! tenant's records cannot be reached through it.
//!
//! [`JoinScaffold`] turns the command line arguments into every decision the
//! generator makes. Like [`ModelScaffold`](super::ModelScaffold) it performs no
//! I/O, so the plan is testable as plain strings.
//!
//! ```
//! use anubis::scaffold::JoinScaffold;
//!
//! let scaffold = JoinScaffold::parse(
//!     "AppliedTag",
//!     "project_id{class_name=Project}",
//!     "tag_id{class_name=Tag}",
//! )?;
//! assert_eq!(scaffold.module(), "applied_tags");
//! assert_eq!(scaffold.owner().pascal(), "Project");
//! assert_eq!(scaffold.target().pascal(), "Tag");
//! # Ok::<(), anubis::scaffold::ScaffoldError>(())
//! ```

use super::error::ScaffoldError;
use super::inflect::Names;
use super::model::{MAX_WIDTH, ROUTER_INDENT, module_pairs, name_pairs};
use super::stamp::Replacements;

/// The living template every `anubis scaffold join` run transforms.
///
/// The join lives in `backend/src/scaffolding/incidentally_linked/` and links
/// the team-owned parent template to `merely_peripheral::PeripheralNotion`, a
/// second team-owned model that exists so the join has a far side. Both are
/// compiled and integration-tested with the rest of the starter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinTemplate {
    join: &'static str,
    owner: &'static str,
    target: &'static str,
    module: &'static str,
    owner_module: &'static str,
    target_module: &'static str,
    frontend: &'static [&'static str],
}

/// The one join template, and the two models it links.
const TEMPLATE: JoinTemplate = JoinTemplate {
    join: "IncidentalLinkage",
    owner: "CreativeConcept",
    target: "PeripheralNotion",
    module: "incidentally_linked",
    owner_module: "absolutely_abstract",
    target_module: "merely_peripheral",
    frontend: &["frontend/src/api/routes/incidentalLinkageRoutes.ts"],
};

impl JoinTemplate {
    /// The template join model's name, e.g. `IncidentalLinkage`.
    #[must_use]
    pub fn join(self) -> &'static str {
        self.join
    }

    /// The template module under `backend/src/scaffolding/`.
    #[must_use]
    pub fn module(self) -> &'static str {
        self.module
    }

    /// The template's frontend files, relative to the application root.
    #[must_use]
    pub fn frontend_files(self) -> &'static [&'static str] {
        self.frontend
    }

    /// The template join's table, e.g. `incidental_linkages`.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn table(self) -> String {
        template_table(self.join)
    }

    /// The table of the template side that owns the association.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name.
    #[must_use]
    pub fn owner_table(self) -> String {
        template_table(self.owner)
    }

    /// The table of the template side the association reaches.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name.
    #[must_use]
    pub fn target_table(self) -> String {
        template_table(self.target)
    }
}

/// The table a template model's name implies.
fn template_table(model: &str) -> String {
    Names::parse(model)
        .expect("template names are valid")
        .snake_plural()
}

/// One planned `anubis scaffold join <JoinModel> <a_id{class_name=A}> <b_id{class_name=B}>`.
///
/// The first side owns the association: its routes carry the attach and detach
/// endpoints, and an `<other>_ids` field scaffolded later lands on it. The
/// second side is the one being linked to.
#[derive(Debug, Clone)]
pub struct JoinScaffold {
    join: Names,
    owner: Names,
    target: Names,
}

impl JoinScaffold {
    /// Plans a join from the command line arguments.
    ///
    /// Each side is written `<model>_id{class_name=<Model>}`, exactly as Bullet
    /// Train writes it. `class` is accepted as a spelling of `class_name`.
    ///
    /// # Errors
    /// Returns an error when a name does not parse, when a side is not
    /// `<attribute>{class_name=<Model>}`, when an attribute is not the class's
    /// own `<model>_id`, or when the three names are not distinct.
    pub fn parse(join: &str, owner: &str, target: &str) -> Result<Self, ScaffoldError> {
        let join = Names::parse(join)?;
        let owner = parse_side(owner)?;
        let target = parse_side(target)?;

        if owner.pascal() == target.pascal() {
            return Err(ScaffoldError::new(format!(
                "a join links two different models, and both sides name `{}`",
                owner.pascal(),
            )));
        }
        for side in [&owner, &target] {
            if side.pascal() == join.pascal() {
                return Err(ScaffoldError::new(format!(
                    "the join model and the models it links must be three different names, and \
                     `{}` is used twice",
                    join.pascal(),
                )));
            }
        }

        Ok(Self {
            join,
            owner,
            target,
        })
    }

    /// The join model's names.
    #[must_use]
    pub fn join(&self) -> &Names {
        &self.join
    }

    /// The side that owns the association, and hosts its endpoints.
    #[must_use]
    pub fn owner(&self) -> &Names {
        &self.owner
    }

    /// The side the association reaches.
    #[must_use]
    pub fn target(&self) -> &Names {
        &self.target
    }

    /// The living template this scaffold transforms.
    #[must_use]
    pub fn template(&self) -> JoinTemplate {
        TEMPLATE
    }

    /// The full rewrite from template names to this join's names.
    ///
    /// Three models are rewritten at once, plus the module paths: generated
    /// code reaches each side through the module that side's own scaffold
    /// created, never through the template it came from.
    #[must_use]
    pub fn replacements(&self) -> Replacements {
        let mut pairs = name_pairs(TEMPLATE.join, &self.join);
        pairs.extend(name_pairs(TEMPLATE.owner, &self.owner));
        pairs.extend(name_pairs(TEMPLATE.target, &self.target));
        pairs.extend(module_pairs(TEMPLATE.module, &self.module()));
        pairs.extend(module_pairs(
            TEMPLATE.owner_module,
            &self.owner.snake_plural(),
        ));
        pairs.extend(module_pairs(
            TEMPLATE.target_module,
            &self.target.snake_plural(),
        ));
        Replacements::new(pairs)
    }

    /// The application module the join lives in, e.g. `applied_tags`.
    #[must_use]
    pub fn module(&self) -> String {
        self.join.snake_plural()
    }

    /// The join table, e.g. `applied_tags`.
    #[must_use]
    pub fn table(&self) -> String {
        self.join.snake_plural()
    }

    /// The migration directory name for `version`.
    #[must_use]
    pub fn migration_directory(&self, version: &str) -> String {
        format!("{version}_create_{}", self.table())
    }

    /// The module declaration inserted into `backend/src/lib.rs`.
    #[must_use]
    pub fn module_declaration(&self) -> String {
        format!("pub mod {};", self.module())
    }

    /// The router mount inserted into `account_router`.
    ///
    /// Pre-wrapped the way `rustfmt` would wrap it when a long name pushes it
    /// past the formatter's width, so generated code is format-clean untouched.
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
}

/// Parses `<attribute>{class_name=<Model>}` into the model it names.
fn parse_side(side: &str) -> Result<Names, ScaffoldError> {
    let Some(open) = side.find('{') else {
        return Err(ScaffoldError::new(format!(
            "side `{side}` must name the model it links: write \
             `<model>_id{{class_name=<Model>}}`"
        )));
    };
    let Some(close) = side.rfind('}').filter(|close| *close > open) else {
        return Err(ScaffoldError::new(format!(
            "side `{side}` opens a modifier list with `{{` and never closes it"
        )));
    };

    let body = side[open + 1..close].trim().trim_matches('"');
    let Some((key, value)) = body.split_once('=') else {
        return Err(ScaffoldError::new(format!(
            "side `{side}` must carry `class_name=<Model>` between its braces"
        )));
    };
    // `class` is what `docs/scaffolding.md` writes in the CLI table and
    // `class_name` is what the `super_select` modifier uses; both read clearly,
    // so both are accepted.
    if !matches!(key.trim(), "class_name" | "class") {
        return Err(ScaffoldError::new(format!(
            "modifier `{}` is not understood on a join side; write `class_name=<Model>`",
            key.trim(),
        )));
    }

    let model = Names::parse(value.trim())?;
    let attribute = side[..open].trim();
    let expected = format!("{}_id", model.snake());
    if attribute != expected {
        return Err(ScaffoldError::new(format!(
            "side `{side}` names its column `{attribute}`, and a join column is named after the \
             model it points at: write `{expected}{{class_name={}}}`",
            model.pascal(),
        )));
    }

    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::JoinScaffold;

    fn applied_tags() -> JoinScaffold {
        JoinScaffold::parse(
            "AppliedTag",
            "project_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        )
        .expect("a valid join")
    }

    #[test]
    fn a_join_names_its_module_table_and_mount() {
        let scaffold = applied_tags();
        assert_eq!(scaffold.module(), "applied_tags");
        assert_eq!(scaffold.table(), "applied_tags");
        assert_eq!(scaffold.module_declaration(), "pub mod applied_tags;");
        assert_eq!(
            scaffold.route_mount(),
            "router = router.merge(applied_tags::router(pool.clone(), roles.clone()));",
        );
        assert_eq!(
            scaffold.migration_directory("2026-08-15-101112"),
            "2026-08-15-101112_create_applied_tags",
        );
    }

    #[test]
    fn the_replacements_rewrite_all_three_models() {
        let replacements = applied_tags().replacements();
        assert_eq!(
            replacements
                .apply("[\"IncidentalLinkage\", \"CreativeConcept\", \"PeripheralNotion\"]"),
            "[\"AppliedTag\", \"Project\", \"Tag\"]",
        );
        assert_eq!(
            replacements.apply("use crate::scaffolding::merely_peripheral::PeripheralNotion;"),
            "use crate::tags::Tag;",
        );
        assert_eq!(
            replacements.apply("IncidentalLinkage::peripheral_notion_ids_by_creative_concept"),
            "AppliedTag::tag_ids_by_project",
        );
        assert_eq!(
            replacements.apply("frontend/src/api/routes/incidentalLinkageRoutes.ts"),
            "frontend/src/api/routes/appliedTagRoutes.ts",
        );
        assert_eq!(
            replacements
                .apply("/account/creative-concepts/{creative_concept_id}/peripheral-notions"),
            "/account/projects/{project_id}/tags",
        );
    }

    #[test]
    fn sides_are_validated() {
        // A side must name its class.
        JoinScaffold::parse("AppliedTag", "project_id", "tag_id{class_name=Tag}").unwrap_err();
        // And the column must be that class's own foreign key.
        let renamed = JoinScaffold::parse(
            "AppliedTag",
            "owner_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        )
        .unwrap_err();
        assert!(
            renamed.message().contains("project_id{class_name=Project}"),
            "the message must name the expected spelling: {}",
            renamed.message(),
        );
        // The three names must be distinct.
        JoinScaffold::parse(
            "AppliedTag",
            "project_id{class_name=Project}",
            "project_id{class_name=Project}",
        )
        .unwrap_err();
        JoinScaffold::parse(
            "Project",
            "project_id{class_name=Project}",
            "tag_id{class_name=Tag}",
        )
        .unwrap_err();
    }

    #[test]
    fn the_shorter_class_spelling_is_accepted_too() {
        let scaffold = JoinScaffold::parse(
            "AppliedTag",
            "project_id{class=Project}",
            "tag_id{class=Tag}",
        )
        .expect("`class` reads as clearly as `class_name`");
        assert_eq!(scaffold.owner().pascal(), "Project");
        assert_eq!(scaffold.target().pascal(), "Tag");
    }
}
