//! Reading declarations back out of an application's own source files.
//!
//! Living templates mean a generator's input is code the application owns, so
//! `anubis scaffold model` lifts the template's `diesel::table!` block out of
//! `backend/src/schema.rs` instead of printing a declaration from a string
//! baked into the framework. Whatever shape the template proves in CI is the
//! shape every generated model inherits, and a developer who improves the
//! template improves every future scaffold with it.
//!
//! Both functions here are lookups over plain text: they return the requested
//! declaration or nothing, and the caller reports the missing file or table.

/// Extracts the `diesel::table!` block declaring `table`, indentation intact.
///
/// The returned block runs from `diesel::table! {` through its closing brace,
/// so it carries the declaration's doc comment and column list unchanged and
/// is ready to hand to [`Replacements::apply`](super::Replacements::apply).
///
/// Matching scans balanced braces, which holds for `table!` declarations: they
/// contain column definitions and comments, never string literals.
///
/// # Examples
/// ```
/// use anubis::scaffold::table_block;
///
/// let schema = "diesel::table! {\n    projects (id) {\n        id -> Uuid,\n    }\n}\n";
/// let block = table_block(schema, "projects").unwrap();
/// assert!(block.starts_with("diesel::table! {"));
/// assert!(block.ends_with('}'));
/// ```
#[must_use]
pub fn table_block(schema: &str, table: &str) -> Option<String> {
    const OPENING: &str = "diesel::table! {";

    let declaration = format!("{table} (");
    let mut search_from = 0;
    while let Some(offset) = schema[search_from..].find(OPENING) {
        let start = search_from + offset;
        let end = closing_brace(&schema[start..])? + start;
        let block = &schema[start..=end];
        if block
            .lines()
            .any(|line| line.trim_start().starts_with(&declaration))
        {
            return Some(block.to_owned());
        }
        search_from = end + 1;
    }
    None
}

/// The offset of the brace closing the first `{` in `source`.
///
/// Shared with [`insert_json_entries`](super::insert_json_entries), which
/// finds a locale file's model object the same way.
pub(super) fn closing_brace(source: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _other => {}
        }
    }
    None
}

/// The first line of `source` containing `needle`, without indentation.
///
/// Single-line declarations (`diesel::joinable!`, the role grants in
/// `roles.yml`) are lifted this way, so the generated line matches the
/// template's exactly once the names are rewritten.
///
/// # Examples
/// ```
/// use anubis::scaffold::line_containing;
///
/// let schema = "// a comment\ndiesel::joinable!(goals -> projects (project_id));\n";
/// assert_eq!(
///     line_containing(schema, "diesel::joinable!(goals").unwrap(),
///     "diesel::joinable!(goals -> projects (project_id));",
/// );
/// ```
#[must_use]
pub fn line_containing(source: &str, needle: &str) -> Option<String> {
    source
        .lines()
        .find(|line| line.contains(needle))
        .map(|line| line.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::{line_containing, table_block};

    const SCHEMA: &str = "\
diesel::table! {
    /// The parent.
    creative_concepts (id) {
        id -> Uuid,
        name -> Text,
    }
}

diesel::table! {
    tangible_things (id) {
        id -> Uuid,
        description -> Nullable<Text>,
    }
}

diesel::joinable!(tangible_things -> creative_concepts (creative_concept_id));
";

    #[test]
    fn extracts_the_requested_table_with_its_doc_comment() {
        let block = table_block(SCHEMA, "creative_concepts").expect("the table is declared");
        assert!(block.starts_with("diesel::table! {"));
        assert!(block.contains("/// The parent."));
        assert!(block.contains("name -> Text,"));
        assert!(!block.contains("tangible_things"));
        assert!(block.ends_with("\n}"));
    }

    #[test]
    fn extracts_a_later_table_past_earlier_blocks() {
        let block = table_block(SCHEMA, "tangible_things").expect("the table is declared");
        assert!(block.contains("description -> Nullable<Text>,"));
        assert!(!block.contains("creative_concepts"));
    }

    #[test]
    fn an_undeclared_table_is_absent() {
        assert!(table_block(SCHEMA, "projects").is_none());
    }

    #[test]
    fn finds_a_single_line_declaration() {
        assert_eq!(
            line_containing(SCHEMA, "diesel::joinable!(tangible_things").unwrap(),
            "diesel::joinable!(tangible_things -> creative_concepts (creative_concept_id));",
        );
        assert!(line_containing(SCHEMA, "diesel::joinable!(projects").is_none());
    }
}
