//! Field types: what a `name:type` scaffold argument means in the database.
//!
//! Bullet Train's field types name a form component; Anubis carries the same
//! vocabulary all the way down, because a statically typed stack needs one
//! answer per type for the column, the Diesel schema, and the Rust field.
//! [`FIELD_TYPES`] is that answer, a table later scaffolders extend by adding
//! a row rather than by adding a branch.
//!
//! Coverage today is the two types the living templates prove end to end:
//! `text_field` (required text) and `text_area` (optional text). Every other
//! type in `docs/scaffolding.md` is rejected by name with the supported list,
//! so no scaffold ever half-generates a type the templates cannot show.

use super::error::ScaffoldError;

/// One supported field type: a column type, a schema type, and a Rust type.
///
/// # Examples
/// ```
/// use anubis::scaffold::FieldType;
///
/// let text_area = FieldType::lookup("text_area").unwrap();
/// assert_eq!(text_area.sql_type(), "TEXT");
/// assert_eq!(text_area.rust_type(), "Option<String>");
/// assert_eq!(text_area.component(), "TextAreaField");
/// assert!(text_area.is_nullable());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldType {
    name: &'static str,
    sql_type: &'static str,
    schema_type: &'static str,
    rust_type: &'static str,
    component: &'static str,
    nullable: bool,
}

/// Every field type `anubis scaffold` understands, in the order it lists them.
pub const FIELD_TYPES: [FieldType; 2] = [
    FieldType {
        name: "text_field",
        sql_type: "TEXT",
        schema_type: "Text",
        rust_type: "String",
        component: "TextField",
        nullable: false,
    },
    FieldType {
        name: "text_area",
        sql_type: "TEXT",
        schema_type: "Text",
        rust_type: "String",
        component: "TextAreaField",
        nullable: true,
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
        if self.nullable {
            format!("Option<{}>", self.rust_type)
        } else {
            self.rust_type.to_owned()
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
    #[must_use]
    pub fn is_nullable(self) -> bool {
        self.nullable
    }
}

/// Column names every scaffolded model already owns.
///
/// The template supplies `id` and both timestamps, and the ownership chain
/// supplies the foreign key, so a field argument naming one of them is a
/// mistake worth catching before anything is written.
const RESERVED: [&str; 4] = ["id", "team_id", "created_at", "updated_at"];

/// One `name:type` argument: a column the scaffolded model carries.
///
/// # Examples
/// ```
/// use anubis::scaffold::Field;
///
/// let field = Field::parse("summary:text_area").unwrap();
/// assert_eq!(field.name(), "summary");
/// assert_eq!(field.sql_column(), "summary TEXT");
/// assert_eq!(field.schema_column(), "summary -> Nullable<Text>,");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    name: String,
    field_type: FieldType,
}

impl Field {
    /// Parses one `name:type` command line argument.
    ///
    /// # Errors
    /// Returns an error when the argument is not `name:type`, when the name is
    /// not a `snake_case` column name, when the name is one the scaffolder
    /// already owns, or when the type is not in [`FIELD_TYPES`].
    pub fn parse(argument: &str) -> Result<Self, ScaffoldError> {
        let Some((name, type_name)) = argument.split_once(':') else {
            return Err(ScaffoldError::new(format!(
                "field `{argument}` must be written as `name:type`, for example `name:text_field`"
            )));
        };

        let name = name.trim();
        validate_name(name)?;

        let Some(field_type) = FieldType::lookup(type_name.trim()) else {
            return Err(ScaffoldError::new(format!(
                "field type `{type_name}` is not supported yet; supported types are {}",
                supported_types(),
            )));
        };

        Ok(Self {
            name: name.to_owned(),
            field_type,
        })
    }

    /// The column name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The field's type.
    #[must_use]
    pub fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// The column definition for a `CREATE TABLE` statement, without a comma.
    #[must_use]
    pub fn sql_column(&self) -> String {
        format!("{} {}", self.name, self.field_type.sql_type())
    }

    /// The column line for a `diesel::table!` block, comma included.
    #[must_use]
    pub fn schema_column(&self) -> String {
        self.schema_column_as(self.field_type.is_nullable())
    }

    /// The column line for a `diesel::table!` block at a chosen nullability.
    ///
    /// A generator that cannot yet write a column has to declare it nullable
    /// whatever the field type says, because a `NOT NULL` column with no
    /// writer fails every insert.
    #[must_use]
    pub fn schema_column_as(&self, nullable: bool) -> String {
        let schema_type = self.field_type.schema_type();
        if nullable {
            format!("{} -> Nullable<{schema_type}>,", self.name)
        } else {
            format!("{} -> {schema_type},", self.name)
        }
    }
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
    if RESERVED.contains(&name) {
        return Err(ScaffoldError::new(format!(
            "field `{name}` is maintained by the scaffolder and cannot be declared"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Field, FieldType};

    #[test]
    fn every_type_maps_to_a_column_and_a_rust_type() {
        let text_field = FieldType::lookup("text_field").expect("text_field is supported");
        assert_eq!(text_field.sql_type(), "TEXT");
        assert_eq!(text_field.schema_type(), "Text");
        assert_eq!(text_field.rust_type(), "String");
        assert_eq!(text_field.component(), "TextField");
        assert!(!text_field.is_nullable());

        let text_area = FieldType::lookup("text_area").expect("text_area is supported");
        assert_eq!(text_area.sql_type(), "TEXT");
        assert_eq!(text_area.schema_type(), "Text");
        assert_eq!(text_area.rust_type(), "Option<String>");
        assert!(text_area.is_nullable());
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
    }

    #[test]
    fn parses_a_field_argument() {
        let field = Field::parse("summary:text_area").expect("valid argument");
        assert_eq!(field.name(), "summary");
        assert_eq!(field.field_type().name(), "text_area");
        assert_eq!(field.sql_column(), "summary TEXT");
        assert_eq!(field.schema_column(), "summary -> Nullable<Text>,");

        let required = Field::parse("name:text_field").expect("valid argument");
        assert_eq!(required.sql_column(), "name TEXT");
        assert_eq!(required.schema_column(), "name -> Text,");
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
}
