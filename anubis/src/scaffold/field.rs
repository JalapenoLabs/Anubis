//! Field types, and how one field propagates through a model's artifacts.
//!
//! Bullet Train's field types name a form component; Anubis carries the same
//! vocabulary all the way down, because a statically typed stack needs one
//! answer per type for the column, the Diesel schema, the Rust field, the wire
//! type, and the React control. [`FIELD_TYPES`] is that answer, a table later
//! scaffolders extend by adding a row rather than by adding a branch.
//!
//! [`FieldScaffold`] turns one `name:type` argument plus the model's names
//! into every line the field contributes, keyed by the artifact that receives
//! it. `anubis scaffold field` inserts those lines into a model that already
//! exists; `anubis scaffold model` inserts the same lines into the artifacts
//! it has just stamped. One definition, both commands, which is why a field
//! declared at scaffold time and a field added a month later land identically.
//!
//! ## The nullable-or-defaulted rule
//!
//! Every column the scaffolder *adds* is safe to add to a table that already
//! holds rows: it is nullable, or it is `NOT NULL` with a database default.
//! `boolean` is the only defaulted type today (`false`), so every other type
//! maps to an `Option` in Rust and to `T | null` on the wire. The living
//! template's own `name` column is required, but that is the template's shape
//! rather than this table's: `text_field` describes a column a scaffold adds.
//!
//! ## Associations
//!
//! One field type declares no column at all. `<other>_ids:super_select{class_name=<Other>}`
//! is Bullet Train's has-many-through spelling, and it names an [`Association`]
//! rather than a [`FieldType`]: the values live in the join table an earlier
//! `anubis scaffold join` run created, so there is no migration and no schema
//! column, and every line the field contributes reads through the join model.

use super::anchor;
use super::error::ScaffoldError;
use super::inflect::Names;

/// One supported field type: a column, a Rust type, a wire type, a control.
///
/// # Examples
/// ```
/// use anubis::scaffold::FieldType;
///
/// let boolean = FieldType::lookup("boolean").unwrap();
/// assert_eq!(boolean.sql_type(), "BOOLEAN");
/// assert_eq!(boolean.rust_type(), "bool");
/// assert_eq!(boolean.component(), "BooleanField");
/// assert!(!boolean.is_nullable());
///
/// let text_area = FieldType::lookup("text_area").unwrap();
/// assert_eq!(text_area.rust_type(), "Option<String>");
/// assert!(text_area.is_nullable());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldType {
    name: &'static str,
    sql_type: &'static str,
    schema_type: &'static str,
    /// The Rust type of a non-null column of this type.
    bare_type: &'static str,
    /// The same value borrowed, for the insertable struct's lifetime.
    borrowed_type: &'static str,
    /// The TypeScript type of a non-null column of this type.
    wire_type: &'static str,
    component: &'static str,
    /// The SQL default that lets the column be `NOT NULL` on an existing
    /// table. `None` means the column is nullable instead.
    default_sql: Option<&'static str>,
    /// How a value of this type is written and read in generated code.
    shape: Shape,
}

/// How generated code handles a value: text needs trimming, the rest does not.
///
/// The shape decides which normalization a handler performs, what the zod
/// schema says, and what an empty form control holds, and it keeps those three
/// answers together so a new field type cannot get one of them wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Trimmed on write; blank clears the column; the form holds `''`.
    Text,
    /// Written as submitted; the form holds `null` until it is filled in.
    Scalar,
    /// Written as submitted; the form holds `false`; never null.
    Boolean,
}

/// Every field type `anubis scaffold` understands, in the order it lists them.
pub const FIELD_TYPES: [FieldType; 5] = [
    FieldType {
        name: "text_field",
        sql_type: "TEXT",
        schema_type: "Text",
        bare_type: "String",
        borrowed_type: "&'a str",
        wire_type: "string",
        component: "TextField",
        default_sql: None,
        shape: Shape::Text,
    },
    FieldType {
        name: "text_area",
        sql_type: "TEXT",
        schema_type: "Text",
        bare_type: "String",
        borrowed_type: "&'a str",
        wire_type: "string",
        component: "TextAreaField",
        default_sql: None,
        shape: Shape::Text,
    },
    FieldType {
        name: "number_field",
        sql_type: "INTEGER",
        schema_type: "Int4",
        bare_type: "i32",
        borrowed_type: "i32",
        wire_type: "number",
        component: "NumberField",
        default_sql: None,
        shape: Shape::Scalar,
    },
    FieldType {
        name: "boolean",
        sql_type: "BOOLEAN",
        schema_type: "Bool",
        bare_type: "bool",
        borrowed_type: "bool",
        wire_type: "boolean",
        component: "BooleanField",
        // A boolean is the one type with an obvious default, which is what
        // lets it be NOT NULL on a table that already holds rows.
        default_sql: Some("false"),
        shape: Shape::Boolean,
    },
    FieldType {
        name: "date_field",
        sql_type: "DATE",
        schema_type: "Date",
        // Written out in full: a generated model never has to gain an import,
        // and chrono is already a dependency of every Anubis application.
        bare_type: "chrono::NaiveDate",
        borrowed_type: "chrono::NaiveDate",
        wire_type: "string",
        component: "DateField",
        default_sql: None,
        shape: Shape::Scalar,
    },
];

impl FieldType {
    /// Looks a field type up by the name used on the command line.
    #[must_use]
    pub fn lookup(name: &str) -> Option<Self> {
        FIELD_TYPES
            .into_iter()
            .find(|candidate| candidate.name == name)
    }

    /// The name used on the command line, e.g. `text_area`.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.name
    }

    /// The PostgreSQL column type, e.g. `TEXT`.
    #[must_use]
    pub fn sql_type(self) -> &'static str {
        self.sql_type
    }

    /// The Diesel schema type, before nullability is applied, e.g. `Text`.
    #[must_use]
    pub fn schema_type(self) -> &'static str {
        self.schema_type
    }

    /// The Rust field type, wrapped in `Option` when the column is nullable.
    #[must_use]
    pub fn rust_type(self) -> String {
        self.wrap(self.bare_type)
    }

    /// The TypeScript type on the wire, `| null` when the column is nullable.
    #[must_use]
    pub fn typescript_type(self) -> String {
        if self.is_nullable() {
            format!("{} | null", self.wire_type)
        } else {
            self.wire_type.to_owned()
        }
    }

    /// The `@jalapenolabs/anubis` field component a form renders for this type.
    ///
    /// The field library is one component per scaffolder field type, so the
    /// type table is where the pairing belongs: a new type adds a row here
    /// rather than a branch in the generator.
    #[must_use]
    pub fn component(self) -> &'static str {
        self.component
    }

    /// Whether the column accepts `NULL`.
    ///
    /// True unless the type carries a database default, which is the whole of
    /// the nullable-or-defaulted rule this module's documentation states.
    #[must_use]
    pub fn is_nullable(self) -> bool {
        self.default_sql.is_none()
    }

    /// Wraps `inner` in `Option` when the column is nullable.
    fn wrap(self, inner: &str) -> String {
        if self.is_nullable() {
            format!("Option<{inner}>")
        } else {
            inner.to_owned()
        }
    }
}

/// The object a model's locale file keeps its column strings in.
///
/// Field strings are namespaced because a column may be called anything: a
/// model with a `title` or an `open` column would otherwise overwrite the
/// page title and the "Open" link its own locale file already declares.
pub const LOCALE_FIELDS: &str = "fields";

/// Column names every scaffolded model already owns.
///
/// The template supplies `id` and both timestamps, and the ownership chain
/// supplies the foreign key, so a field argument naming one of them is a
/// mistake worth catching before anything is written.
const RESERVED: [&str; 4] = ["id", "team_id", "created_at", "updated_at"];

/// The field type that declares an association rather than a column.
const ASSOCIATION_TYPE: &str = "super_select";

/// The suffix that makes an association plural, as in `tag_ids`.
const ASSOCIATION_SUFFIX: &str = "_ids";

/// A has-many-through association, backed by a join model.
///
/// The target is the model on the other side, as the command's
/// `{class_name=...}` modifier names it. The join is the model connecting the
/// two, which only the application knows, so the CLI resolves it and hands it
/// back through [`Field::through`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Association {
    target: Names,
    join: Option<Names>,
}

impl Association {
    /// The model on the other side of the join, e.g. `Tag`.
    #[must_use]
    pub fn target(&self) -> &Names {
        &self.target
    }

    /// The join model backing the association, once the CLI has found it.
    #[must_use]
    pub fn join(&self) -> Option<&Names> {
        self.join.as_ref()
    }
}

/// What a field argument declares: a column, or an association through a join.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldKind {
    Column(FieldType),
    Association(Association),
}

/// One `name:type` argument: a column, or an association, the model carries.
///
/// # Examples
/// ```
/// use anubis::scaffold::Field;
///
/// let field = Field::parse("summary:text_area").unwrap();
/// assert_eq!(field.name(), "summary");
/// assert_eq!(field.sql_column().as_deref(), Some("summary TEXT"));
/// assert_eq!(field.schema_column().as_deref(), Some("summary -> Nullable<Text>,"));
///
/// let flag = Field::parse("archived:boolean").unwrap();
/// assert_eq!(flag.sql_column().as_deref(), Some("archived BOOLEAN NOT NULL DEFAULT false"));
/// assert_eq!(flag.schema_column().as_deref(), Some("archived -> Bool,"));
///
/// let tags = Field::parse("tag_ids:super_select{class_name=Tag}").unwrap();
/// assert_eq!(tags.association().map(|through| through.target().pascal()), Some("Tag".to_owned()));
/// assert!(tags.sql_column().is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    name: String,
    kind: FieldKind,
}

impl Field {
    /// Parses one `name:type` command line argument, modifiers included.
    ///
    /// # Errors
    /// Returns an error when the argument is not `name:type`, when the name is
    /// not a `snake_case` column name, when the name is one the scaffolder
    /// already owns, when a modifier list is malformed, or when the type is
    /// neither `super_select` nor a member of [`FIELD_TYPES`].
    pub fn parse(argument: &str) -> Result<Self, ScaffoldError> {
        let (declaration, modifiers) = split_modifiers(argument)?;

        let Some((name, type_name)) = declaration.split_once(':') else {
            return Err(ScaffoldError::new(format!(
                "field `{argument}` must be written as `name:type`, for example `name:text_field`"
            )));
        };

        let name = name.trim();
        validate_name(name)?;
        let type_name = type_name.trim();

        if type_name == ASSOCIATION_TYPE {
            return parse_association(name, &modifiers);
        }
        if let Some((key, _value)) = modifiers.first() {
            return Err(ScaffoldError::new(format!(
                "modifier `{key}` is not understood on `{type_name}`; modifiers land on \
                 `super_select` today, as `{ASSOCIATION_TYPE}{{class_name=<Other>}}`"
            )));
        }

        let Some(field_type) = FieldType::lookup(type_name) else {
            return Err(ScaffoldError::new(format!(
                "field type `{type_name}` is not supported yet; supported types are {}",
                supported_types(),
            )));
        };

        Ok(Self {
            name: name.to_owned(),
            kind: FieldKind::Column(field_type),
        })
    }

    /// Names the join model backing this field's association.
    ///
    /// Only the application knows which join connects two models, so the CLI
    /// finds it and completes the field before anything is planned.
    #[must_use]
    pub fn through(mut self, join: Names) -> Self {
        if let FieldKind::Association(association) = &mut self.kind {
            association.join = Some(join);
        }
        self
    }

    /// The column name, or the association's `<other>_ids` attribute.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The column's type, or `None` when the field is an association.
    #[must_use]
    pub fn field_type(&self) -> Option<FieldType> {
        match &self.kind {
            FieldKind::Column(field_type) => Some(*field_type),
            FieldKind::Association(_through) => None,
        }
    }

    /// The association this field declares, if it declares one.
    #[must_use]
    pub fn association(&self) -> Option<&Association> {
        match &self.kind {
            FieldKind::Column(_field_type) => None,
            FieldKind::Association(association) => Some(association),
        }
    }

    /// The type name as the command line spells it.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            FieldKind::Column(field_type) => field_type.name,
            FieldKind::Association(_through) => ASSOCIATION_TYPE,
        }
    }

    /// The column definition for a `CREATE TABLE` statement, without a comma.
    ///
    /// `None` for an association: its values live in the join table.
    #[must_use]
    pub fn sql_column(&self) -> Option<String> {
        let field_type = self.field_type()?;
        match field_type.default_sql {
            None => Some(format!("{} {}", self.name, field_type.sql_type)),
            Some(default) => Some(format!(
                "{} {} NOT NULL DEFAULT {default}",
                self.name, field_type.sql_type,
            )),
        }
    }

    /// The column line for a `diesel::table!` block, comma included.
    ///
    /// `None` for an association, which adds no column to this model's table.
    #[must_use]
    pub fn schema_column(&self) -> Option<String> {
        let field_type = self.field_type()?;
        let schema_type = field_type.schema_type;
        if field_type.is_nullable() {
            Some(format!("{} -> Nullable<{schema_type}>,", self.name))
        } else {
            Some(format!("{} -> {schema_type},", self.name))
        }
    }
}

/// A field argument split into its `name:type` half and its modifiers.
type Declaration<'argument> = (&'argument str, Vec<(String, String)>);

/// Splits a trailing `{key=value,...}` modifier list off a field argument.
///
/// Bullet Train also allows the whole list to be quoted, so a modifier value
/// may contain a comma; the quotes are stripped and the rest reads the same.
fn split_modifiers(argument: &str) -> Result<Declaration<'_>, ScaffoldError> {
    let Some(open) = argument.find('{') else {
        return Ok((argument, Vec::new()));
    };
    let Some(close) = argument.rfind('}').filter(|close| *close > open) else {
        return Err(ScaffoldError::new(format!(
            "field `{argument}` opens a modifier list with `{{` and never closes it"
        )));
    };

    let mut modifiers = Vec::new();
    for entry in argument[open + 1..close]
        .trim()
        .trim_matches('"')
        .split(',')
    {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((key, value)) = entry.split_once('=') else {
            return Err(ScaffoldError::new(format!(
                "modifier `{entry}` must be written as `key=value`"
            )));
        };
        modifiers.push((key.trim().to_owned(), value.trim().to_owned()));
    }

    Ok((&argument[..open], modifiers))
}

/// Builds the association a `super_select{class_name=...}` argument names.
fn parse_association(name: &str, modifiers: &[(String, String)]) -> Result<Field, ScaffoldError> {
    let mut target = None;
    for (key, value) in modifiers {
        match key.as_str() {
            "class_name" => target = Some(value.as_str()),
            _other => {
                return Err(ScaffoldError::new(format!(
                    "modifier `{key}` is not understood on `{ASSOCIATION_TYPE}` yet; `class_name` \
                     is the only one, and `source` is on the roadmap"
                )));
            }
        }
    }

    let Some(target) = target else {
        return Err(ScaffoldError::new(format!(
            "`{ASSOCIATION_TYPE}` needs the model it selects from: write \
             `<other>{ASSOCIATION_SUFFIX}:{ASSOCIATION_TYPE}{{class_name=<Other>}}`"
        )));
    };
    let target = Names::parse(target)?;

    let Some(base) = name.strip_suffix(ASSOCIATION_SUFFIX) else {
        return Err(ScaffoldError::new(format!(
            "field `{name}` names a single `{}`, which is a belongs_to association and is not \
             generated yet. A has-many-through association carries the plural suffix: \
             `{}{ASSOCIATION_SUFFIX}:{ASSOCIATION_TYPE}{{class_name={}}}`",
            target.pascal(),
            target.snake(),
            target.pascal(),
        )));
    };
    if base != target.snake() {
        return Err(ScaffoldError::new(format!(
            "field `{name}` does not match `class_name={}`; a has-many-through association is \
             named after the model it reaches, so write `{}{ASSOCIATION_SUFFIX}`",
            target.pascal(),
            target.snake(),
        )));
    }

    Ok(Field {
        name: name.to_owned(),
        kind: FieldKind::Association(Association { target, join: None }),
    })
}

/// The supported type names, rendered for an error message.
fn supported_types() -> String {
    FIELD_TYPES
        .iter()
        .map(|field_type| field_type.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rejects names that are not usable as a column and a Rust field at once.
fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    if name.is_empty() {
        return Err(ScaffoldError::new("a field name is empty".to_owned()));
    }
    if !name.starts_with(|character: char| character.is_ascii_lowercase()) {
        return Err(ScaffoldError::new(format!(
            "field `{name}` must start with a lowercase ASCII letter"
        )));
    }
    if let Some(bad) = name.chars().find(|character| {
        !character.is_ascii_lowercase() && !character.is_ascii_digit() && *character != '_'
    }) {
        return Err(ScaffoldError::new(format!(
            "field `{name}` contains `{bad}`; use lowercase letters, digits, and underscores"
        )));
    }
    if name.contains("__") || name.ends_with('_') {
        return Err(ScaffoldError::new(format!(
            "field `{name}` has an empty word; separate words with a single underscore"
        )));
    }
    if RESERVED.contains(&name) {
        return Err(ScaffoldError::new(format!(
            "field `{name}` is maintained by the scaffolder and cannot be declared"
        )));
    }
    Ok(())
}

/// One artifact of a model's slice, as a destination for field insertions.
///
/// Which artifacts a model has depends on its ownership depth: a team-owned
/// model has a list page and a show page, a nested model has one section
/// component instead. The caller knows which files exist; this enum only says
/// what each kind of file receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// `backend/src/<models>/model.rs`.
    Model,
    /// `backend/src/<models>/routes.rs`.
    Routes,
    /// `backend/tests/<models>_flow.rs`.
    Test,
    /// `frontend/src/api/routes/<model>Routes.ts`.
    ApiRoutes,
    /// `frontend/src/components/<Model>Form.tsx`.
    Form,
    /// A table of the model's records: its list page or its section component.
    Table,
    /// `frontend/src/pages/<Model>Page.tsx`.
    ShowPage,
}

/// One field, planned against one model: every line it contributes.
///
/// # Examples
/// ```
/// use anubis::scaffold::{Artifact, Field, FieldScaffold, Names};
///
/// let scaffold = FieldScaffold::new(
///     Names::parse("Project")?,
///     Field::parse("priority:text_field")?,
/// );
/// assert_eq!(scaffold.label(), "Priority");
/// assert_eq!(
///     scaffold.add_column("projects").as_deref(),
///     Some("ALTER TABLE projects ADD COLUMN priority TEXT;"),
/// );
///
/// let insertions = scaffold.insertions(Artifact::Model);
/// assert!(insertions.iter().any(|(anchor, line)| {
///     anchor.contains("record-fields") && line.contains("pub priority: Option<String>,")
/// }));
/// # Ok::<(), anubis::scaffold::ScaffoldError>(())
/// ```
#[derive(Debug, Clone)]
pub struct FieldScaffold {
    model: Names,
    field: Field,
    /// The translated label, e.g. `Due date`.
    label: String,
    /// The translated help text under the control.
    help: String,
    /// The i18next key the label and help live under, e.g. `projects.dueDate`.
    locale_key: String,
    /// The last segment of that key, e.g. `dueDate`.
    locale_name: String,
}

impl FieldScaffold {
    /// Plans one field against the model that carries it.
    ///
    /// # Panics
    /// Panics if `field` carries a name [`Field::parse`] would have rejected,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn new(model: Names, field: Field) -> Self {
        // A field name is a name like any other, so the same inflector that
        // turns `TangibleThing` into prose turns `due_date` into "Due date".
        let words = Names::parse(field.name()).expect("a parsed field name is a parseable name");
        // An association is labelled after the model it reaches, because
        // "Tag ids" is the column's name and "Tags" is the field's meaning.
        let (label, help) = match field.association() {
            None => {
                let label = words.human();
                let help = format!("{label} of the {}.", model.lower());
                (label, help)
            }
            Some(association) => {
                let label = upper_first(&association.target().lower_plural());
                let help = format!("{label} linked to this {}.", model.lower());
                (label, help)
            }
        };
        let locale_name = words.camel();
        let locale_key = format!("{}.{LOCALE_FIELDS}.{locale_name}", model.camel_plural());
        Self {
            model,
            field,
            label,
            help,
            locale_key,
            locale_name,
        }
    }

    /// The field itself.
    #[must_use]
    pub fn field(&self) -> &Field {
        &self.field
    }

    /// The column name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.field.name()
    }

    /// The translated label, e.g. `Due date` for `due_date`.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The `ALTER TABLE` statement that adds the column.
    ///
    /// `None` for an association: the join table already holds its values, so
    /// the run writes no migration at all.
    #[must_use]
    pub fn add_column(&self, table: &str) -> Option<String> {
        let column = self.field.sql_column()?;
        Some(format!("ALTER TABLE {table} ADD COLUMN {column};"))
    }

    /// The `ALTER TABLE` statement that removes the column again.
    #[must_use]
    pub fn drop_column(&self, table: &str) -> Option<String> {
        self.field.field_type()?;
        Some(format!("ALTER TABLE {table} DROP COLUMN {};", self.name()))
    }

    /// The column's type, on the paths only a column field reaches.
    ///
    /// # Panics
    /// Panics when called for an association, which is a bug in the generator
    /// rather than in an application: [`insertions`](Self::insertions) routes
    /// associations to their own methods before any of these run.
    fn column_type(&self) -> FieldType {
        self.field
            .field_type()
            .expect("only a column field reaches this path")
    }

    /// The field's strings, as they sit inside the model's [`LOCALE_FIELDS`]
    /// object, label first.
    ///
    /// Keys are `camelCase` like every other key in a model's locale file,
    /// even though the column and the form control keep the wire's
    /// `snake_case` name.
    #[must_use]
    pub fn locale_entries(&self) -> Vec<(String, String)> {
        vec![
            (self.locale_name.clone(), self.label.clone()),
            (format!("{}Help", self.locale_name), self.help.clone()),
        ]
    }

    /// Every `(anchor, lines)` pair this field inserts into `artifact`.
    ///
    /// The order is the order the caller should apply them in, which only
    /// matters for readability: each anchor is independent.
    #[must_use]
    pub fn insertions(&self, artifact: Artifact) -> Vec<(&'static str, String)> {
        if let Some(association) = self.field.association() {
            return self.association_insertions(artifact, association);
        }
        match artifact {
            Artifact::Model => self.model_insertions(),
            Artifact::Routes => self.routes_insertions(),
            Artifact::Test => self.test_insertions(),
            Artifact::ApiRoutes => self.api_routes_insertions(),
            Artifact::Form => self.form_insertions(),
            Artifact::Table => self.table_insertions(),
            Artifact::ShowPage => vec![(anchor::SHOW_FIELDS, self.show_row())],
        }
    }

    /// Every `(anchor, lines)` pair an association contributes to `artifact`.
    ///
    /// An association owns no column, so `model.rs` receives nothing: the ids
    /// live in the join table, and every line here reads or writes them
    /// through the join model an earlier `anubis scaffold join` run generated.
    ///
    /// # Panics
    /// Panics when the association has no join, which is a bug in the CLI: it
    /// resolves the join out of the application before planning anything.
    fn association_insertions(
        &self,
        artifact: Artifact,
        association: &Association,
    ) -> Vec<(&'static str, String)> {
        let join = association
            .join()
            .expect("the CLI resolves an association's join before planning");
        let target = association.target();
        let name = self.name();
        let record = self.model.snake();

        match artifact {
            // The ids are the join table's rows, not this model's columns.
            Artifact::Model => Vec::new(),
            Artifact::Routes => self.association_routes(join, target),
            // The narrative proves the wire shape; the join's own generated
            // test is where attaching and detaching are proven.
            Artifact::Test => vec![
                (anchor::TEST_CREATE, format!("\"{name}\": [],")),
                (
                    anchor::TEST_CREATED,
                    format!("assert_eq!(body[\"{record}\"][\"{name}\"], json!([]));"),
                ),
            ],
            Artifact::ApiRoutes => vec![
                (anchor::WIRE_FIELDS, format!("{name}: string[]")),
                (anchor::CREATE_REQUEST, format!("{name}?: string[]")),
                (
                    anchor::UPDATE_REQUEST,
                    format!("/** Replaces the whole set. */\n{name}?: string[]"),
                ),
            ],
            Artifact::Form => self.association_form(join, target),
            // A table shows how many are linked; the records themselves are a
            // click away on the model's own page.
            Artifact::Table => vec![
                (
                    anchor::LIST_COLUMNS,
                    format!(
                        "<TableColumn>{{\n    t('{key}')\n  }}</TableColumn>",
                        key = self.locale_key,
                    ),
                ),
                (
                    anchor::LIST_CELLS,
                    format!(
                        "<TableCell>{{\n    {}.{name}.length\n  }}</TableCell>",
                        self.model.camel(),
                    ),
                ),
            ],
            Artifact::ShowPage => vec![(
                anchor::SHOW_FIELDS,
                format!(
                    "<div>\n  <dt className='text-sm opacity-60'>{{\n      t('{key}')\n    \
                     }}</dt>\n  <dd>{{\n      record?.{name}.length ?? 0\n    }}</dd>\n</div>",
                    key = self.locale_key,
                ),
            )],
        }
    }

    /// The request bodies, the reconciliations, and the serialized ids.
    fn association_routes(&self, join: &Names, target: &Names) -> Vec<(&'static str, String)> {
        let name = self.name();
        let module = join.snake_plural();
        let join_type = join.pascal();
        let loader = format!("{}_ids_by_{}", target.snake(), self.model.snake());

        // The record and its team are both in scope wherever this lands: a
        // team-owned model carries `team_id`, and a join links two of them.
        let reconcile = format!(
            "if let Some(requested) = body.{name}.as_deref() {{\n    \
             crate::{module}::{join_type}::replace_all(\n        &mut connection,\n        \
             record.id,\n        record.team_id,\n        requested,\n    )\n    .await?;\n}}",
        );

        vec![
            (
                anchor::CREATE_BODY,
                format!("/// {}\n{name}: Option<Vec<Uuid>>,", self.help),
            ),
            (
                anchor::UPDATE_BODY,
                format!(
                    "/// {} Absent leaves the set alone; a list replaces it whole.\n\
                     {name}: Option<Vec<Uuid>>,",
                    self.help,
                ),
            ),
            (anchor::CREATE_ASSOCIATIONS, reconcile.clone()),
            (anchor::UPDATE_ASSOCIATIONS, reconcile),
            (
                anchor::VIEW_FIELDS,
                format!("/// {}\n{name}: Vec<Uuid>,", self.help),
            ),
            (
                anchor::VIEW_LOAD,
                format!(
                    "let {name} = crate::{module}::{join_type}::{loader}(\n    connection,\n    \
                     &records.iter().map(|record| record.id).collect::<Vec<_>>(),\n)\n.await?;",
                ),
            ),
            (
                anchor::VIEW_VALUES,
                format!("{name}: {name}.get(&record.id).cloned().unwrap_or_default(),"),
            ),
        ]
    }

    /// The import, the options hook, the schema, the value, and the control.
    fn association_form(&self, join: &Names, target: &Names) -> Vec<(&'static str, String)> {
        let name = self.name();
        let key = &self.locale_key;
        let options = format!("{}Options", target.camel());
        let hook = format!("use{}Options", join.pascal());

        vec![
            (anchor::FIELD_IMPORTS, "SuperSelectField,".to_owned()),
            (
                anchor::FORM_IMPORTS,
                format!(
                    "import {{ {hook} }} from '../api/routes/{}Routes'",
                    join.camel(),
                ),
            ),
            (
                anchor::FORM_HOOKS,
                format!("const {options} = {hook}(props.teamId)"),
            ),
            (anchor::FORM_SCHEMA, format!("{name}: z.array(z.string()),")),
            (
                anchor::FORM_VALUES,
                format!("{name}: editing?.{name} ?? [],"),
            ),
            (anchor::FORM_PAYLOAD, format!("{name}: data.{name},")),
            (
                anchor::FORM_FIELDS,
                format!(
                    "<SuperSelectField\n  control={{form.control}}\n  name='{name}'\n  \
                     label={{t('{key}')}}\n  help={{t('{key}Help')}}\n  isMultiple\n  \
                     options={{{options}}}\n/>",
                ),
            ),
        ]
    }

    /// The record, insertable, and changeset structs, plus the empty test.
    fn model_insertions(&self) -> Vec<(&'static str, String)> {
        let name = self.name();
        let field_type = self.column_type();
        vec![
            (
                anchor::RECORD_FIELDS,
                format!("/// {}\npub {name}: {},", self.help, field_type.rust_type()),
            ),
            (
                anchor::INSERT_FIELDS,
                format!(
                    "/// {}\npub {name}: {},",
                    self.help,
                    field_type.wrap(field_type.borrowed_type),
                ),
            ),
            (
                anchor::CHANGESET_FIELDS,
                format!(
                    "/// New {}\npub {name}: Option<{}>,",
                    lower_first(&self.help),
                    field_type.rust_type(),
                ),
            ),
            (
                anchor::CHANGESET_EMPTY,
                format!("if self.{name}.is_some() {{\n    return false;\n}}"),
            ),
        ]
    }

    /// The request bodies, the normalizations, and the two struct literals.
    fn routes_insertions(&self) -> Vec<(&'static str, String)> {
        let name = self.name();
        let field_type = self.column_type();
        // A request body is optional whatever the column is: absent means
        // "unchanged" on update, and "leave it at its default" on create.
        let submitted = format!("Option<{}>", field_type.bare_type);

        let mut insertions = vec![
            (
                anchor::CREATE_BODY,
                format!("/// {}\n{name}: {submitted},", self.help),
            ),
            (
                anchor::UPDATE_BODY,
                format!("/// {}\n{name}: {submitted},", self.help),
            ),
        ];

        match field_type.shape {
            Shape::Text => {
                insertions.push((
                    anchor::CREATE_NORMALIZE,
                    format!(
                        "let {name} = body.{name}.as_deref().map(str::trim)\
                         .filter(|value| !value.is_empty());"
                    ),
                ));
                insertions.push((anchor::INSERT_VALUES, format!("{name},")));
                insertions.push((
                    anchor::UPDATE_NORMALIZE,
                    format!("let {name} = optional_text(body.{name}.as_deref());"),
                ));
                insertions.push((anchor::CHANGESET_VALUES, format!("{name},")));
            }
            Shape::Scalar => {
                insertions.push((anchor::INSERT_VALUES, format!("{name}: body.{name},")));
                insertions.push((
                    anchor::UPDATE_NORMALIZE,
                    format!("let {name} = body.{name}.map(Some);"),
                ));
                insertions.push((anchor::CHANGESET_VALUES, format!("{name},")));
            }
            Shape::Boolean => {
                let default = field_type.default_sql.unwrap_or("false");
                insertions.push((
                    anchor::INSERT_VALUES,
                    format!("{name}: body.{name}.unwrap_or({default}),"),
                ));
                insertions.push((anchor::CHANGESET_VALUES, format!("{name}: body.{name},")));
            }
        }

        insertions
    }

    /// The created and updated halves of the model's narrative test.
    fn test_insertions(&self) -> Vec<(&'static str, String)> {
        let name = self.name();
        let record = self.model.snake();
        let (created, updated) = self.samples();
        vec![
            (anchor::TEST_CREATE, format!("\"{name}\": {created},")),
            (
                anchor::TEST_CREATED,
                format!("assert_eq!(body[\"{record}\"][\"{name}\"], json!({created}));"),
            ),
            (anchor::TEST_UPDATE, format!("\"{name}\": {updated},")),
            (
                anchor::TEST_UPDATED,
                format!("assert_eq!(body[\"{record}\"][\"{name}\"], json!({updated}));"),
            ),
        ]
    }

    /// The wire type and the two request types.
    fn api_routes_insertions(&self) -> Vec<(&'static str, String)> {
        let name = self.name();
        let field_type = self.column_type();
        // A request member is optional and, for a nullable column, explicitly
        // nullable: sending null is how a form clears a value it cannot blank.
        let submitted = if field_type.is_nullable() && field_type.shape != Shape::Text {
            format!("{name}?: {} | null", field_type.wire_type)
        } else {
            format!("{name}?: {}", field_type.wire_type)
        };
        vec![
            (
                anchor::WIRE_FIELDS,
                format!("{name}: {}", field_type.typescript_type()),
            ),
            (anchor::CREATE_REQUEST, submitted.clone()),
            (
                anchor::UPDATE_REQUEST,
                if field_type.shape == Shape::Text {
                    format!(
                        "/** A blank {} clears the column. */\n{submitted}",
                        self.lower_label()
                    )
                } else {
                    submitted
                },
            ),
        ]
    }

    /// The import, the schema, the values, the payload, and the control.
    fn form_insertions(&self) -> Vec<(&'static str, String)> {
        let name = self.name();
        let field_type = self.column_type();
        let key = &self.locale_key;

        let (schema, empty, payload) = match field_type.shape {
            Shape::Text => ("z.string()", "''", format!("{name}: data.{name}.trim(),")),
            Shape::Scalar if field_type.wire_type == "number" => (
                "z.number().nullable()",
                "null",
                format!("{name}: data.{name},"),
            ),
            Shape::Scalar => (
                "z.string().nullable()",
                "null",
                format!("{name}: data.{name},"),
            ),
            Shape::Boolean => ("z.boolean()", "false", format!("{name}: data.{name},")),
        };

        let mut control = format!(
            "<{component}\n  control={{form.control}}\n  name='{name}'\n  \
             label={{t('{key}')}}\n  help={{t('{key}Help')}}",
            component = field_type.component,
        );
        if field_type.component == "TextAreaField" {
            control.push_str("\n  minRows={2}");
        }
        control.push_str("\n/>");

        vec![
            (anchor::FIELD_IMPORTS, format!("{},", field_type.component)),
            (anchor::FORM_SCHEMA, format!("{name}: {schema},")),
            (
                anchor::FORM_VALUES,
                format!("{name}: editing?.{name} ?? {empty},"),
            ),
            (anchor::FORM_PAYLOAD, payload),
            (anchor::FORM_FIELDS, control),
        ]
    }

    /// The column header and the cell of a table of the model's records.
    fn table_insertions(&self) -> Vec<(&'static str, String)> {
        let key = &self.locale_key;
        vec![
            (
                anchor::LIST_COLUMNS,
                format!("<TableColumn>{{\n    t('{key}')\n  }}</TableColumn>"),
            ),
            (
                anchor::LIST_CELLS,
                format!("<TableCell>{{\n    {}\n  }}</TableCell>", self.cell_value()),
            ),
        ]
    }

    /// One attribute of the record, as a show page renders it.
    fn show_row(&self) -> String {
        let key = &self.locale_key;
        let value = match self.column_type().shape {
            Shape::Boolean => format!("record?.{} ? t('common.yes') : t('common.no')", self.name()),
            Shape::Text | Shape::Scalar => format!("record?.{}", self.name()),
        };
        format!(
            "<div>\n  <dt className='text-sm opacity-60'>{{\n      t('{key}')\n    \
             }}</dt>\n  <dd>{{\n      {value}\n    }}</dd>\n</div>",
        )
    }

    /// The expression a table cell renders for one record.
    fn cell_value(&self) -> String {
        let record = self.model.camel();
        match self.column_type().shape {
            Shape::Boolean => format!(
                "{record}.{} ? t('common.yes') : t('common.no')",
                self.name()
            ),
            Shape::Text | Shape::Scalar => format!("{record}.{}", self.name()),
        }
    }

    /// The two JSON values the generated test writes and then overwrites.
    ///
    /// They differ so that the update half of the narrative proves the column
    /// actually changed rather than merely survived.
    fn samples(&self) -> (&'static str, &'static str) {
        match self.column_type().name {
            "number_field" => ("3", "5"),
            "boolean" => ("true", "false"),
            "date_field" => ("\"2026-01-31\"", "\"2026-02-28\""),
            _text => ("\"Alpha\"", "\"Beta\""),
        }
    }

    /// The label as it reads mid-sentence, e.g. `due date`.
    fn lower_label(&self) -> String {
        self.label.to_lowercase()
    }
}

/// Uppercases the first character of a phrase so it can start a sentence.
fn upper_first(phrase: &str) -> String {
    let mut raised = phrase.to_owned();
    if let Some(first) = raised.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    raised
}

/// Lowercases the first character of a sentence so it can be prefixed.
fn lower_first(sentence: &str) -> String {
    let mut lowered = sentence.to_owned();
    if let Some(first) = lowered.get_mut(0..1) {
        first.make_ascii_lowercase();
    }
    lowered
}

#[cfg(test)]
mod tests {
    use super::super::inflect::Names;
    use super::{Artifact, Field, FieldScaffold, FieldType};

    fn scaffold(argument: &str) -> FieldScaffold {
        FieldScaffold::new(
            Names::parse("Project").expect("a valid model name"),
            Field::parse(argument).expect("a valid field argument"),
        )
    }

    fn line(scaffold: &FieldScaffold, artifact: Artifact, anchor: &str) -> String {
        scaffold
            .insertions(artifact)
            .into_iter()
            .find(|(candidate, _lines)| candidate.contains(anchor))
            .unwrap_or_else(|| panic!("no insertion for {anchor}"))
            .1
    }

    #[test]
    fn every_type_maps_to_a_column_and_a_rust_type() {
        let text_field = FieldType::lookup("text_field").expect("text_field is supported");
        assert_eq!(text_field.sql_type(), "TEXT");
        assert_eq!(text_field.schema_type(), "Text");
        assert_eq!(text_field.rust_type(), "Option<String>");
        assert_eq!(text_field.typescript_type(), "string | null");
        assert_eq!(text_field.component(), "TextField");
        assert!(text_field.is_nullable());

        let number = FieldType::lookup("number_field").expect("number_field is supported");
        assert_eq!(number.sql_type(), "INTEGER");
        assert_eq!(number.rust_type(), "Option<i32>");
        assert_eq!(number.typescript_type(), "number | null");

        let date = FieldType::lookup("date_field").expect("date_field is supported");
        assert_eq!(date.rust_type(), "Option<chrono::NaiveDate>");
        assert_eq!(date.typescript_type(), "string | null");
    }

    #[test]
    fn a_defaulted_type_is_the_one_that_is_not_null() {
        let boolean = FieldType::lookup("boolean").expect("boolean is supported");
        assert!(!boolean.is_nullable());
        assert_eq!(boolean.rust_type(), "bool");
        assert_eq!(boolean.typescript_type(), "boolean");

        let field = Field::parse("archived:boolean").expect("a valid argument");
        assert_eq!(
            field.sql_column().as_deref(),
            Some("archived BOOLEAN NOT NULL DEFAULT false"),
        );
        assert_eq!(field.schema_column().as_deref(), Some("archived -> Bool,"));
    }

    #[test]
    fn unknown_types_list_the_supported_ones() {
        assert!(FieldType::lookup("color_picker").is_none());
        let error = Field::parse("shade:color_picker").unwrap_err();
        assert!(
            error.message().contains("text_field, text_area"),
            "message must list the supported types: {}",
            error.message(),
        );
        assert!(error.message().contains("date_field"));
    }

    #[test]
    fn parses_a_field_argument() {
        let field = Field::parse("summary:text_area").expect("valid argument");
        assert_eq!(field.name(), "summary");
        assert_eq!(field.type_name(), "text_area");
        assert_eq!(field.sql_column().as_deref(), Some("summary TEXT"));
        assert_eq!(
            field.schema_column().as_deref(),
            Some("summary -> Nullable<Text>,"),
        );
    }

    #[test]
    fn rejects_malformed_arguments() {
        for argument in [
            "summary",
            ":text_field",
            "Summary:text_field",
            "9lives:text_field",
            "sum mary:text_field",
            "id:text_field",
            "created_at:text_field",
            "team_id:text_field",
        ] {
            Field::parse(argument).unwrap_err();
        }
    }

    #[test]
    fn a_text_field_reaches_every_backend_artifact() {
        let field = scaffold("priority:text_field");
        assert_eq!(field.label(), "Priority");
        assert_eq!(
            field.add_column("projects").as_deref(),
            Some("ALTER TABLE projects ADD COLUMN priority TEXT;"),
        );
        assert_eq!(
            field.drop_column("projects").as_deref(),
            Some("ALTER TABLE projects DROP COLUMN priority;"),
        );

        assert!(
            line(&field, Artifact::Model, "record-fields")
                .contains("pub priority: Option<String>,")
        );
        assert!(
            line(&field, Artifact::Model, "insert-fields")
                .contains("pub priority: Option<&'a str>,")
        );
        assert!(
            line(&field, Artifact::Model, "changeset-fields")
                .contains("pub priority: Option<Option<String>>,")
        );
        assert_eq!(
            line(&field, Artifact::Model, "changeset-empty"),
            "if self.priority.is_some() {\n    return false;\n}",
        );

        assert!(
            line(&field, Artifact::Routes, "create-body").contains("priority: Option<String>,")
        );
        assert!(
            line(&field, Artifact::Routes, "create-normalize")
                .contains("body.priority.as_deref().map(str::trim)")
        );
        assert_eq!(line(&field, Artifact::Routes, "insert-values"), "priority,");
        assert_eq!(
            line(&field, Artifact::Routes, "update-normalize"),
            "let priority = optional_text(body.priority.as_deref());",
        );
        assert_eq!(
            line(&field, Artifact::Routes, "changeset-values"),
            "priority,"
        );

        assert_eq!(
            line(&field, Artifact::Test, "test-create"),
            "\"priority\": \"Alpha\",",
        );
        assert_eq!(
            line(&field, Artifact::Test, "test-updated"),
            "assert_eq!(body[\"project\"][\"priority\"], json!(\"Beta\"));",
        );
    }

    #[test]
    fn a_text_field_reaches_every_frontend_artifact() {
        let field = scaffold("priority:text_field");

        assert_eq!(
            line(&field, Artifact::ApiRoutes, "wire-fields"),
            "priority: string | null",
        );
        assert_eq!(
            line(&field, Artifact::ApiRoutes, "create-request"),
            "priority?: string",
        );
        assert!(
            line(&field, Artifact::ApiRoutes, "update-request")
                .contains("A blank priority clears the column.")
        );

        assert_eq!(line(&field, Artifact::Form, "field-imports"), "TextField,");
        assert_eq!(
            line(&field, Artifact::Form, "form-schema"),
            "priority: z.string(),",
        );
        assert_eq!(
            line(&field, Artifact::Form, "form-values"),
            "priority: editing?.priority ?? '',",
        );
        assert_eq!(
            line(&field, Artifact::Form, "form-payload"),
            "priority: data.priority.trim(),",
        );
        let control = line(&field, Artifact::Form, "form-fields");
        assert!(control.starts_with("<TextField\n"), "{control}");
        assert!(control.contains("label={t('projects.fields.priority')}"));
        assert!(control.contains("help={t('projects.fields.priorityHelp')}"));

        assert!(
            line(&field, Artifact::Table, "list-columns").contains("t('projects.fields.priority')"),
        );
        assert!(line(&field, Artifact::Table, "list-cells").contains("project.priority"));
        assert!(line(&field, Artifact::ShowPage, "show-fields").contains("record?.priority"));
    }

    #[test]
    fn a_boolean_is_written_and_read_without_an_option() {
        let field = scaffold("archived:boolean");

        assert!(line(&field, Artifact::Model, "record-fields").contains("pub archived: bool,"));
        assert!(
            line(&field, Artifact::Model, "changeset-fields")
                .contains("pub archived: Option<bool>,")
        );
        assert_eq!(
            line(&field, Artifact::Routes, "insert-values"),
            "archived: body.archived.unwrap_or(false),",
        );
        assert_eq!(
            line(&field, Artifact::Routes, "changeset-values"),
            "archived: body.archived,",
        );
        assert!(
            field
                .insertions(Artifact::Routes)
                .iter()
                .all(|(anchor, _lines)| !anchor.contains("normalize")),
            "a boolean needs no normalization",
        );
        assert_eq!(
            line(&field, Artifact::ApiRoutes, "wire-fields"),
            "archived: boolean",
        );
        assert_eq!(
            line(&field, Artifact::Form, "form-values"),
            "archived: editing?.archived ?? false,",
        );
        assert!(line(&field, Artifact::Table, "list-cells").contains("t('common.yes')"));
    }

    #[test]
    fn a_scalar_carries_no_trimming_and_an_explicit_null() {
        let number = scaffold("urgency:number_field");
        assert_eq!(
            line(&number, Artifact::Routes, "insert-values"),
            "urgency: body.urgency,",
        );
        assert_eq!(
            line(&number, Artifact::Routes, "update-normalize"),
            "let urgency = body.urgency.map(Some);",
        );
        assert_eq!(
            line(&number, Artifact::ApiRoutes, "update-request"),
            "urgency?: number | null",
        );
        assert_eq!(
            line(&number, Artifact::Form, "form-schema"),
            "urgency: z.number().nullable(),",
        );

        let date = scaffold("due_date:date_field");
        assert_eq!(date.label(), "Due date");
        assert!(
            line(&date, Artifact::Model, "record-fields")
                .contains("pub due_date: Option<chrono::NaiveDate>,")
        );
        assert_eq!(
            line(&date, Artifact::Form, "form-schema"),
            "due_date: z.string().nullable(),",
        );
        assert_eq!(
            date.locale_entries(),
            vec![
                ("dueDate".to_owned(), "Due date".to_owned()),
                (
                    "dueDateHelp".to_owned(),
                    "Due date of the project.".to_owned(),
                ),
            ],
        );
        assert!(
            line(&date, Artifact::Form, "form-fields").contains("t('projects.fields.dueDate')")
        );
    }

    /// The association a `scaffold join AppliedTag ...` run would back.
    fn association() -> FieldScaffold {
        let field = Field::parse("tag_ids:super_select{class_name=Tag}")
            .expect("a valid association argument")
            .through(Names::parse("AppliedTag").expect("a valid join name"));
        FieldScaffold::new(Names::parse("Project").expect("a valid model name"), field)
    }

    #[test]
    fn an_association_declares_no_column() {
        let field = Field::parse("tag_ids:super_select{class_name=Tag}").expect("a valid argument");
        assert_eq!(field.type_name(), "super_select");
        assert!(field.field_type().is_none());
        assert!(field.sql_column().is_none());
        assert!(field.schema_column().is_none());
        assert_eq!(
            field.association().map(|through| through.target().pascal()),
            Some("Tag".to_owned()),
        );

        let scaffold = association();
        assert_eq!(
            scaffold.label(),
            "Tags",
            "an association reads as its target"
        );
        assert!(scaffold.add_column("projects").is_none());
        assert!(scaffold.drop_column("projects").is_none());
        assert!(scaffold.insertions(Artifact::Model).is_empty());
        assert_eq!(
            scaffold.locale_entries(),
            vec![
                ("tagIds".to_owned(), "Tags".to_owned()),
                (
                    "tagIdsHelp".to_owned(),
                    "Tags linked to this project.".to_owned()
                ),
            ],
        );
    }

    #[test]
    fn an_association_reads_and_writes_through_its_join() {
        let scaffold = association();

        assert_eq!(
            line(&scaffold, Artifact::Routes, "create-body"),
            "/// Tags linked to this project.\ntag_ids: Option<Vec<Uuid>>,",
        );
        assert!(
            line(&scaffold, Artifact::Routes, "create-associations")
                .contains("crate::applied_tags::AppliedTag::replace_all(")
        );
        assert!(
            line(&scaffold, Artifact::Routes, "update-associations").contains("record.team_id,")
        );
        assert!(
            line(&scaffold, Artifact::Routes, "view-load")
                .contains("AppliedTag::tag_ids_by_project(")
        );
        assert_eq!(
            line(&scaffold, Artifact::Routes, "view-values"),
            "tag_ids: tag_ids.get(&record.id).cloned().unwrap_or_default(),",
        );

        assert_eq!(
            line(&scaffold, Artifact::ApiRoutes, "wire-fields"),
            "tag_ids: string[]",
        );
        assert_eq!(
            line(&scaffold, Artifact::Form, "form-imports"),
            "import { useAppliedTagOptions } from '../api/routes/appliedTagRoutes'",
        );
        assert_eq!(
            line(&scaffold, Artifact::Form, "form-hooks"),
            "const tagOptions = useAppliedTagOptions(props.teamId)",
        );
        assert_eq!(
            line(&scaffold, Artifact::Form, "form-values"),
            "tag_ids: editing?.tag_ids ?? [],",
        );
        let control = line(&scaffold, Artifact::Form, "form-fields");
        assert!(control.starts_with("<SuperSelectField\n"), "{control}");
        assert!(control.contains("isMultiple"));
        assert!(control.contains("options={tagOptions}"));

        assert!(line(&scaffold, Artifact::Table, "list-cells").contains("project.tag_ids.length"));
        assert!(
            line(&scaffold, Artifact::ShowPage, "show-fields")
                .contains("record?.tag_ids.length ?? 0")
        );
    }

    #[test]
    fn association_arguments_are_validated() {
        // `class_name` is required, and only it is understood today.
        Field::parse("tag_ids:super_select").unwrap_err();
        Field::parse("tag_ids:super_select{source=team.tags}").unwrap_err();
        // A modifier list has to close, and each entry has to be `key=value`.
        Field::parse("tag_ids:super_select{class_name=Tag").unwrap_err();
        Field::parse("tag_ids:super_select{class_name}").unwrap_err();
        // Modifiers land on `super_select`, not on a column type.
        Field::parse("priority:text_field{class_name=Tag}").unwrap_err();

        // The plural suffix is what separates has-many-through from belongs_to.
        let single = Field::parse("tag:super_select{class_name=Tag}").unwrap_err();
        assert!(
            single.message().contains("belongs_to"),
            "the singular form must name what it is: {}",
            single.message(),
        );
        assert!(single.message().contains("tag_ids:super_select"));

        // And the name has to match the class it reaches.
        let mismatched = Field::parse("label_ids:super_select{class_name=Tag}").unwrap_err();
        assert!(
            mismatched.message().contains("tag_ids"),
            "the message must name the expected spelling: {}",
            mismatched.message(),
        );

        // Bullet Train's quoted modifier list reads the same.
        Field::parse("tag_ids:super_select{\"class_name=Tag\"}").expect("quotes are accepted");
    }
}
