//! Text transformation: ordered replacements and magic-anchor insertion.
//!
//! Two mechanisms carry all of Anubis scaffolding, exactly as in Bullet
//! Train's Super Scaffolding:
//!
//! - [`Replacements`] rewrites template names into target names. Pairs apply
//!   longest-first so `tangible_things` wins over `tangible_thing`, and a
//!   whole-tree stamp is just this transform over every path and file body.
//! - [`insert_above_anchor`] edits files the application already owns. A
//!   magic anchor comment (`// 🐺 anubis:routes`) marks the insertion point;
//!   new lines land above it with the anchor's indentation, and inserting the
//!   same block twice is a no-op so scaffolds stay idempotent.
//!
//! Both operate on plain strings; callers do the file I/O and add path
//! context to errors.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use super::inflect::Names;

/// An ordered set of find-and-replace pairs, applied longest-first.
///
/// # Examples
/// ```
/// use anubis::scaffold::Replacements;
///
/// let replacements = Replacements::new([
///     ("anubis-starter".to_owned(), "acme".to_owned()),
///     ("anubis-starter-frontend".to_owned(), "acme-frontend".to_owned()),
/// ]);
/// assert_eq!(replacements.apply("anubis-starter-frontend"), "acme-frontend");
/// assert_eq!(replacements.apply("cargo run -p anubis-starter"), "cargo run -p acme");
/// ```
#[derive(Debug, Clone)]
pub struct Replacements {
    /// Pairs sorted longest-pattern-first so overlapping patterns never
    /// clobber each other (`tangible_things` must match before
    /// `tangible_thing`).
    pairs: Vec<(String, String)>,
}

impl Replacements {
    /// Builds a replacement set from `(from, to)` pairs.
    #[must_use]
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut pairs = pairs.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| {
            right
                .0
                .len()
                .cmp(&left.0.len())
                .then_with(|| left.0.cmp(&right.0))
        });
        Self { pairs }
    }

    /// The full name-variant mapping from one model name to another.
    ///
    /// Covers every [`Names`] variant, so a template written around
    /// `TangibleThing` transforms cleanly into any target model.
    #[must_use]
    pub fn between(template: &Names, target: &Names) -> Self {
        Self::new([
            (template.pascal(), target.pascal()),
            (template.pascal_plural(), target.pascal_plural()),
            (template.camel(), target.camel()),
            (template.camel_plural(), target.camel_plural()),
            (template.snake(), target.snake()),
            (template.snake_plural(), target.snake_plural()),
            (template.kebab(), target.kebab()),
            (template.kebab_plural(), target.kebab_plural()),
            (template.title(), target.title()),
            (template.title_plural(), target.title_plural()),
            (template.human(), target.human()),
            (template.lower(), target.lower()),
            (template.lower_plural(), target.lower_plural()),
        ])
    }

    /// Applies every pair to `input`, longest pattern first.
    #[must_use]
    pub fn apply(&self, input: &str) -> String {
        let mut output = input.to_owned();
        for (from, to) in &self.pairs {
            output = output.replace(from, to);
        }
        output
    }

    /// Consumes the set, yielding its `(from, to)` pairs.
    ///
    /// Scaffolders combine several sets, one per name being rewritten: a
    /// nested model transforms both its own template name and its parent's.
    /// Feed the concatenated pairs back through [`Replacements::new`] so the
    /// merged set is ordered longest-first again.
    #[must_use]
    pub fn into_pairs(self) -> Vec<(String, String)> {
        self.pairs
    }
}

/// Inserts `addition` above the line containing `anchor`, keeping indentation.
///
/// The anchor line's leading whitespace prefixes every inserted line, so the
/// insertion matches the surrounding block. Inserting a block that is already
/// present returns the content unchanged, which keeps scaffold commands
/// idempotent. The rest of the file is preserved byte-for-byte.
///
/// # Examples
/// ```
/// use anubis::scaffold::insert_above_anchor;
///
/// let source = "fn routes() {\n    // 🐺 anubis:routes\n}\n";
/// let updated = insert_above_anchor(source, "🐺 anubis:routes", "route_a();").unwrap();
/// assert_eq!(updated, "fn routes() {\n    route_a();\n    // 🐺 anubis:routes\n}\n");
///
/// // A second identical insert is a no-op.
/// assert_eq!(insert_above_anchor(&updated, "🐺 anubis:routes", "route_a();").unwrap(), updated);
/// ```
///
/// # Errors
/// Returns an error when `anchor` does not occur in `content`.
pub fn insert_above_anchor(
    content: &str,
    anchor: &str,
    addition: &str,
) -> Result<String, AnchorError> {
    let Some(anchor_offset) = content.find(anchor) else {
        return Err(AnchorError::new(anchor));
    };

    // The insertion point is the start of the anchor's line; its leading
    // whitespace becomes the indentation for every inserted line.
    let line_start = content[..anchor_offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let indentation: String = content[line_start..]
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect();

    let mut block = String::new();
    for line in addition.lines() {
        if line.is_empty() {
            block.push('\n');
        } else {
            block.push_str(&indentation);
            block.push_str(line);
            block.push('\n');
        }
    }

    if content.contains(&block) {
        return Ok(content.to_owned());
    }

    let mut updated = String::with_capacity(content.len() + block.len());
    updated.push_str(&content[..line_start]);
    updated.push_str(&block);
    updated.push_str(&content[line_start..]);
    Ok(updated)
}

/// A missing magic-anchor comment.
pub struct AnchorError {
    anchor: String,
    backtrace: Backtrace,
}

impl AnchorError {
    fn new(anchor: &str) -> Self {
        Self {
            anchor: anchor.to_owned(),
            backtrace: Backtrace::capture(),
        }
    }

    /// The anchor text that was not found.
    #[must_use]
    pub fn anchor(&self) -> &str {
        &self.anchor
    }
}

impl fmt::Debug for AnchorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnchorError")
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}

impl Display for AnchorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "anchor comment `{}` not found", self.anchor)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for AnchorError {}

#[cfg(test)]
mod tests {
    use super::super::inflect::Names;
    use super::{Replacements, insert_above_anchor};

    #[test]
    fn longest_pattern_wins() {
        let replacements = Replacements::new([
            ("tangible_thing".to_owned(), "invoice".to_owned()),
            ("tangible_things".to_owned(), "invoices".to_owned()),
        ]);
        assert_eq!(
            replacements.apply("mod tangible_things; use tangible_thing::TangibleThing;"),
            "mod invoices; use invoice::TangibleThing;"
        );
    }

    #[test]
    fn between_two_models_covers_every_variant() {
        let template = Names::parse("TangibleThing").unwrap();
        let target = Names::parse("PurchaseOrder").unwrap();
        let replacements = Replacements::between(&template, &target);

        let source = "\
struct TangibleThing; // Tangible Thing
let tangibleThings = list_tangible_things();
GET /tangible-things
A Tangible thing.";
        let expected = "\
struct PurchaseOrder; // Purchase Order
let purchaseOrders = list_purchase_orders();
GET /purchase-orders
A Purchase order.";
        assert_eq!(replacements.apply(source), expected);
    }

    #[test]
    fn inserts_with_matching_indentation() {
        let source = "fn router() {\n        // 🐺 anubis:routes\n}\n";
        let updated = insert_above_anchor(source, "🐺 anubis:routes", "mount_a();").unwrap();
        assert_eq!(
            updated,
            "fn router() {\n        mount_a();\n        // 🐺 anubis:routes\n}\n"
        );
    }

    #[test]
    fn multi_line_additions_indent_every_line() {
        let source = "    // 🐺 anubis:nav\n";
        let updated =
            insert_above_anchor(source, "🐺 anubis:nav", "<Item\n  to=\"/projects\"\n/>").unwrap();
        assert_eq!(
            updated,
            "    <Item\n      to=\"/projects\"\n    />\n    // 🐺 anubis:nav\n"
        );
    }

    #[test]
    fn repeated_insert_is_idempotent() {
        let source = "list:\n  # 🐺 anubis:entries\n";
        let once = insert_above_anchor(source, "🐺 anubis:entries", "- alpha").unwrap();
        let twice = insert_above_anchor(&once, "🐺 anubis:entries", "- alpha").unwrap();
        assert_eq!(once, twice);
        assert_eq!(once.matches("- alpha").count(), 1);
    }

    #[test]
    fn missing_anchor_is_an_error() {
        let error =
            insert_above_anchor("no anchors here\n", "🐺 anubis:routes", "x();").unwrap_err();
        assert_eq!(error.anchor(), "🐺 anubis:routes");
    }

    #[test]
    fn anchor_on_first_line_inserts_at_the_top() {
        let source = "// 🐺 anubis:top\nrest\n";
        let updated = insert_above_anchor(source, "🐺 anubis:top", "first").unwrap();
        assert_eq!(updated, "first\n// 🐺 anubis:top\nrest\n");
    }
}
