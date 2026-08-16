//! The ownership escape hatch behind `anubis eject`.
//!
//! Bullet Train's `bin/resolve --eject` copies a framework file into the
//! application so the developer owns it. This module is the Anubis equivalent
//! for the frontend package: it names the files an application may take
//! ownership of, and performs the two text transforms an ejection needs.
//!
//! - [`CATALOG`] and [`find`] name the ejectable surface: the field component
//!   library, which is the package's presentation layer. Everything else the
//!   package ships (the API client, the realtime client, the React hooks, the
//!   WebAuthn helpers) speaks a protocol the backend keeps moving, so a copy of
//!   it would silently fork the wire contract instead of restyling a control.
//! - [`eject_module`] rewrites a copied file's own imports: a relative import
//!   whose names the package re-exports becomes an import of the package, and
//!   every other relative import is left alone, because the ejected tree
//!   mirrors the package's directory layout and those specifiers still resolve.
//! - [`rewrite_import`] moves one name out of an application's
//!   `@jalapenolabs/anubis` import onto a local one, splitting a shared import
//!   line rather than rewriting the whole file.
//! - [`root_exports`] reads the package's own `index.ts` for the names it
//!   re-exports, so the first transform is decided by the package rather than
//!   by a list here that would drift away from it.
//!
//! Everything here is pure string-to-string transformation; file discovery,
//! resolution through `node_modules`, and writing belong to the CLI.
//!
//! ```
//! use anubis::eject::{eject_module, rewrite_import};
//!
//! let ejected = eject_module(
//!     "// Copyright\n\nimport { FieldWrapper } from './FieldWrapper'\n",
//!     "// Ejected from ...\n",
//!     &["./FieldWrapper".to_owned()],
//! );
//! assert!(ejected.contains("from '@jalapenolabs/anubis'"));
//!
//! let consumer = "import { TextAreaField, TextField } from '@jalapenolabs/anubis'\n";
//! let rewired = rewrite_import(consumer, "TextField", "../anubis/fields/TextField").unwrap();
//! assert_eq!(
//!     rewired,
//!     "import { TextAreaField } from '@jalapenolabs/anubis'\n\
//!      import { TextField } from '../anubis/fields/TextField'\n",
//! );
//! ```

use std::collections::BTreeSet;

/// The npm package the framework's frontend ships as.
pub const PACKAGE: &str = "@jalapenolabs/anubis";

/// Where an ejected file lands, relative to the application root.
///
/// The package's own layout continues under it (`src/fields/TextField.tsx`
/// becomes `frontend/src/anubis/fields/TextField.tsx`), which is what keeps a
/// copied file's relative imports of its siblings valid, and what makes the
/// provenance of every file in the directory obvious at a glance.
pub const DESTINATION: &str = "frontend/src/anubis";

/// The line width the frontend's lint bar holds imports to.
const MAX_LINE: usize = 120;

/// One file an application may take ownership of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    /// The exported symbol, which is also the argument `anubis eject` takes.
    pub key: &'static str,
    /// The file, relative to the package root.
    pub source: &'static str,
    /// The one line the catalog prints.
    pub summary: &'static str,
}

/// Every component `anubis eject` knows, ordered as `--list` prints them.
///
/// The surface is the field component library and the two pieces every field
/// composes. A field is presentation: what a control looks like, how a label
/// sits above it, and which `HeroUI` component backs it are decisions an
/// application's design has every right to make, and taking one file over
/// costs nothing but that file's future improvements.
pub const CATALOG: [Component; 21] = [
    Component {
        key: "BooleanField",
        source: "src/fields/BooleanField.tsx",
        summary: "the `boolean` field, a HeroUI Switch",
    },
    Component {
        key: "ButtonsField",
        source: "src/fields/ButtonsField.tsx",
        summary: "the `buttons` field, a segmented single choice",
    },
    Component {
        key: "CodeEditorField",
        source: "src/fields/CodeEditorField.tsx",
        summary: "the `code_editor` field, CodeMirror 6 behind a lazy import",
    },
    Component {
        key: "ColorPickerField",
        source: "src/fields/ColorPickerField.tsx",
        summary: "the `color_picker` field, an input plus a native swatch",
    },
    Component {
        key: "DateAndTimeField",
        source: "src/fields/DateAndTimeField.tsx",
        summary: "the `date_and_time_field`, storing UTC and editing local",
    },
    Component {
        key: "DateField",
        source: "src/fields/DateField.tsx",
        summary: "the `date_field`, storing `YYYY-MM-DD` verbatim",
    },
    Component {
        key: "EmailField",
        source: "src/fields/EmailField.tsx",
        summary: "the `email_field`, with the email keyboard and autofill",
    },
    Component {
        key: "EmojiField",
        source: "src/fields/EmojiField.tsx",
        summary: "the `emoji_field`, one grapheme from the platform's keyboard",
    },
    Component {
        key: "FieldWrapper",
        source: "src/fields/FieldWrapper.tsx",
        summary: "the layout every field shares: label, control, help or error",
    },
    Component {
        key: "FileField",
        source: "src/fields/FileField.tsx",
        summary: "the `file_field`, uploading through the application's endpoint",
    },
    Component {
        key: "ImageField",
        source: "src/fields/ImageField.tsx",
        summary: "the `image` field, a file field showing what was chosen",
    },
    Component {
        key: "NumberField",
        source: "src/fields/NumberField.tsx",
        summary: "the `number_field`, holding a number or null",
    },
    Component {
        key: "OptionsField",
        source: "src/fields/OptionsField.tsx",
        summary: "the `options` field, a select or a radio group",
    },
    Component {
        key: "PasswordField",
        source: "src/fields/PasswordField.tsx",
        summary: "the `password_field`, masked",
    },
    Component {
        key: "PhoneField",
        source: "src/fields/PhoneField.tsx",
        summary: "the `phone_field`, with the dial keyboard",
    },
    Component {
        key: "RichTextField",
        source: "src/fields/RichTextField.tsx",
        summary: "the `rich_text` field, Tiptap behind a lazy import",
    },
    Component {
        key: "RichTextView",
        source: "src/fields/RichTextView.tsx",
        summary: "the sanitized render of a `rich_text` value on a page",
    },
    Component {
        key: "SuperSelectField",
        source: "src/fields/SuperSelectField.tsx",
        summary: "the `super_select` field, single or multiple with chips",
    },
    Component {
        key: "TextAreaField",
        source: "src/fields/TextAreaField.tsx",
        summary: "the `text_area` field, a HeroUI Textarea",
    },
    Component {
        key: "TextField",
        source: "src/fields/TextField.tsx",
        summary: "the `text_field`, a single line of free text",
    },
    Component {
        key: "useFieldState",
        source: "src/fields/useFieldState.ts",
        summary: "the hook every field binds through, and its error policy",
    },
];

/// The component `key` names, if the catalog holds one.
#[must_use]
pub fn find(key: &str) -> Option<Component> {
    CATALOG.into_iter().find(|component| component.key == key)
}

/// One `import` or `export` statement, and where it sits in the module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The module it reads from, exactly as the source spells it.
    pub specifier: String,
    /// The names between its braces, empty when it has none.
    pub names: Vec<String>,
    /// Whether it is a type-only statement.
    pub is_type: bool,
    start: usize,
    end: usize,
}

impl Statement {
    /// Whether every name this statement binds is public API of the package.
    ///
    /// A statement with no braced list binds nothing this module can reason
    /// about, so it is never public: the file it reads from rides along into
    /// the application rather than being left pointing at the package.
    #[must_use]
    pub fn is_public(&self, exports: &BTreeSet<String>) -> bool {
        let mut names = self
            .names
            .iter()
            .filter(|item| !is_comment(item))
            .peekable();
        names.peek().is_some() && names.all(|item| exports.contains(bound(item)))
    }
}

/// The name a clause item binds, which is its first identifier.
///
/// `TextField as Text` is bound as `TextField`, so a consumer that renamed an
/// import is still rewired when that component is ejected.
fn bound(item: &str) -> &str {
    item.split_whitespace().next().unwrap_or(item)
}

/// Whether a clause item is a comment rather than a name.
///
/// A scaffolded form imports its field components through a list closed by the
/// `🐺 anubis:field-imports` anchor, so an import clause is one of the few
/// places a comment sits between names. Ejecting a field out of such a list
/// must leave the anchor where it is, or the next `anubis scaffold field` run
/// would have nowhere to insert.
fn is_comment(item: &str) -> bool {
    item.starts_with("//")
}

/// The name a clause item exports, which is its last identifier.
fn exported(item: &str) -> &str {
    item.split_whitespace().next_back().unwrap_or(item)
}

/// Every relative import in a module, static and dynamic, in source order.
///
/// Relative imports are the ones an ejection has to decide about: an import of
/// `react` or `@heroui/react` resolves the same from the application as it did
/// from the package, so it is left exactly as it was.
///
/// The dynamic ones matter as much as the static ones, and are easier to miss:
/// the heavy field components reach their editors through `import(...)` inside
/// a `lazy(...)` factory, so a walk that only read `import` statements would
/// copy `RichTextField.tsx` into an application and leave the editor it
/// fetches at runtime behind. They bind no names, which is exactly right: a
/// module reached only dynamically is never public API of the package, so it
/// always rides along into the application rather than being read back out of
/// the package.
#[must_use]
pub fn relative_imports(contents: &str) -> Vec<Statement> {
    let mut imports = parse(contents, "import ");
    for specifier in dynamic_specifiers(contents) {
        if imports.iter().all(|import| import.specifier != specifier) {
            imports.push(Statement {
                specifier,
                names: Vec::new(),
                is_type: false,
                // A dynamic import is never rewritten: it names no exports, so
                // it can never be read from the package, and these two are
                // only ever consulted for a statement that is.
                start: 0,
                end: 0,
            });
        }
    }

    imports.retain(|import| import.specifier.starts_with('.'));
    imports
}

/// The module specifier of every `import('...')` expression in a module.
fn dynamic_specifiers(contents: &str) -> Vec<String> {
    let mut specifiers = Vec::new();
    let mut rest = contents;

    while let Some(offset) = rest.find("import(") {
        rest = &rest[offset + "import(".len()..];
        // Only a plain quoted specifier is understood, which is the only kind
        // a bundler can split anyway: `import(someVariable)` is skipped rather
        // than guessed at.
        let Some(quoted) = rest.strip_prefix('\'') else {
            continue;
        };
        let Some(length) = quoted.find('\'') else {
            continue;
        };
        specifiers.push(quoted[..length].to_owned());
        rest = &quoted[length + 1..];
    }

    specifiers
}

/// The names the package's `index.ts` re-exports.
///
/// This is what decides whether an ejected file may keep reading a dependency
/// from the package: a name the package root exports is public API and stays
/// where it is, and a name it does not is internal and rides along into the
/// application. Reading the answer out of the package keeps it correct as the
/// package's own surface changes.
#[must_use]
pub fn root_exports(index: &str) -> BTreeSet<String> {
    parse(index, "export ")
        .into_iter()
        .flat_map(|statement| statement.names)
        .map(|item| exported(&item).to_owned())
        .collect()
}

/// The provenance note stamped into every ejected file.
#[must_use]
pub fn banner(source: &str, version: &str, date: &str) -> String {
    format!(
        "// Ejected from {PACKAGE} v{version} ({source}) on {date}.\n\
         // This file belongs to this application now: upstream improvements to it no longer\n\
         // arrive. Delete it to go back to the package's copy.\n",
    )
}

/// One copied file, with its provenance stamped and its imports rewritten.
///
/// `package_imports` names the relative specifiers that become imports of the
/// package; every other import is left untouched. Statements that end up
/// reading from the package are merged into the last of them, so a file never
/// carries the same module on two lines, and the comment groups the package
/// author wrote survive the transform.
#[must_use]
pub fn eject_module(contents: &str, banner: &str, package_imports: &[String]) -> String {
    let imports = parse(contents, "import ");
    let rewritten = imports
        .iter()
        .filter(|import| package_imports.contains(&import.specifier))
        .collect::<Vec<_>>();

    let mut edits = Vec::new();
    for is_type in [false, true] {
        let group = rewritten
            .iter()
            .filter(|import| import.is_type == is_type)
            .collect::<Vec<_>>();
        let Some((last, earlier)) = group.split_last() else {
            continue;
        };

        let mut names = group
            .iter()
            .flat_map(|import| import.names.clone())
            .collect::<Vec<_>>();
        // Alphabetical, with any comment the clause carried last, where a
        // scaffolder's anchor belongs: it closes the list it inserts into.
        names.sort_by(|left, right| (is_comment(left), left).cmp(&(is_comment(right), right)));
        names.dedup();

        edits.push((last.start, last.end, Some(render(is_type, &names, PACKAGE))));
        edits.extend(
            earlier
                .iter()
                .map(|import| (import.start, import.end, None)),
        );
    }

    with_banner(&splice(contents, edits), banner)
}

/// Moves one name out of the application's package import onto a local one.
///
/// Returns `None` when the file does not import that name from the package,
/// which is how the caller reports what it rewired without diffing files.
/// Other names on the same line stay with the package, on their own line.
#[must_use]
pub fn rewrite_import(contents: &str, name: &str, local: &str) -> Option<String> {
    let mut edits = Vec::new();
    for import in parse(contents, "import ") {
        if import.specifier != PACKAGE {
            continue;
        }
        let Some(position) = import.names.iter().position(|item| bound(item) == name) else {
            continue;
        };

        let mut remaining = import.names.clone();
        let moved = remaining.remove(position);
        let mut replacement = render(import.is_type, &[moved], local);
        if !remaining.is_empty() {
            replacement = format!(
                "{}\n{replacement}",
                render(import.is_type, &remaining, PACKAGE),
            );
        }
        edits.push((import.start, import.end, Some(replacement)));
    }

    if edits.is_empty() {
        return None;
    }
    Some(splice(contents, edits))
}

/// Every `import`/`export` statement introduced by `keyword`, in source order.
///
/// Statements are recognized at the start of a line, which is where the
/// frontend's style guide puts them, and end at the closing quote of their
/// specifier, so a braced list wrapped over several lines is one statement.
fn parse(contents: &str, keyword: &str) -> Vec<Statement> {
    let mut statements = Vec::new();
    let mut start = 0;

    for line in contents.split_inclusive('\n') {
        let statement_start = start;
        start += line.len();
        if !line.starts_with(keyword) {
            continue;
        }

        // The clause runs to its closing brace, which may sit several lines
        // below. The specifier is looked for after it rather than from the
        // start of the statement, because a comment between the braces may
        // hold an apostrophe of its own.
        let rest = &contents[statement_start..];
        let clause_end = match line.find('{') {
            None => keyword.len(),
            Some(brace) => match rest[brace..].find('}') {
                None => continue,
                Some(close) => brace + close + 1,
            },
        };

        let Some(offset) = rest[clause_end..].find('\'') else {
            continue;
        };
        let open = clause_end + offset;
        let Some(length) = rest[open + 1..].find('\'') else {
            continue;
        };

        let clause = &rest[keyword.len()..clause_end];
        statements.push(Statement {
            specifier: rest[open + 1..open + 1 + length].to_owned(),
            names: names_in(clause),
            is_type: clause.trim_start().starts_with("type"),
            start: statement_start,
            end: statement_start + open + length + 2,
        });
    }

    statements
}

/// The names a braced clause binds, in the order it writes them.
fn names_in(clause: &str) -> Vec<String> {
    let Some(open) = clause.find('{') else {
        return Vec::new();
    };
    let Some(close) = clause.rfind('}') else {
        return Vec::new();
    };

    clause[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One import statement, wrapped when it is too long or carries a comment.
///
/// A clause holding a comment always wraps: on one line the comment would
/// swallow the rest of the statement.
fn render(is_type: bool, names: &[String], specifier: &str) -> String {
    let keyword = if is_type { "import type" } else { "import" };

    let line = format!("{keyword} {{ {} }} from '{specifier}'", names.join(", "));
    if line.len() <= MAX_LINE && !names.iter().any(|name| is_comment(name)) {
        return line;
    }

    let mut wrapped = format!("{keyword} {{\n");
    for name in names {
        wrapped.push_str("  ");
        wrapped.push_str(name);
        wrapped.push_str(if is_comment(name) { "\n" } else { ",\n" });
    }
    wrapped.push_str("} from '");
    wrapped.push_str(specifier);
    wrapped.push('\'');
    wrapped
}

/// Applies replacements and deletions to `contents`, by byte span.
///
/// A deletion takes the statement's trailing newline with it, so removing a
/// merged import leaves no blank line where it stood.
fn splice(contents: &str, mut edits: Vec<(usize, usize, Option<String>)>) -> String {
    edits.sort_by_key(|(start, _end, _replacement)| *start);

    let mut spliced = String::with_capacity(contents.len());
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        spliced.push_str(&contents[cursor..start]);
        cursor = end;
        match replacement {
            Some(text) => spliced.push_str(&text),
            None => {
                if contents[cursor..].starts_with('\n') {
                    cursor += 1;
                }
            }
        }
    }
    spliced.push_str(&contents[cursor..]);
    spliced
}

/// Stamps the provenance note under the file's copyright header.
///
/// The header stays the first line because the frontend's `ESLint` requires it
/// there; the note follows it, above the imports.
fn with_banner(contents: &str, banner: &str) -> String {
    match contents.split_once("\n\n") {
        Some((header, rest)) if header.starts_with("// Copyright") => {
            format!("{header}\n\n{banner}\n{rest}")
        }
        _ => format!("{banner}\n{contents}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        CATALOG, MAX_LINE, banner, eject_module, find, relative_imports, rewrite_import,
        root_exports,
    };

    /// The catalog names files the package actually ships.
    #[test]
    fn every_catalog_entry_names_a_file_in_the_package() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../frontend");
        for component in CATALOG {
            assert!(
                package.join(component.source).is_file(),
                "{} names {}, which the package does not ship",
                component.key,
                component.source,
            );
        }
    }

    /// Keys are unique, sorted, and are the names the package exports them as.
    #[test]
    fn the_catalog_is_a_sorted_set_of_exported_names() {
        let keys = CATALOG.map(|component| component.key);
        let mut sorted = keys;
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "the catalog prints in key order");

        let mut unique = keys.to_vec();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "keys are unique");

        let index = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../frontend/src/index.ts"),
        )
        .expect("the package's index is readable");
        let exports = root_exports(&index);
        for key in keys {
            assert!(exports.contains(key), "the package does not export {key}");
        }

        assert_eq!(
            find("TextField").map(|found| found.source),
            Some("src/fields/TextField.tsx")
        );
        assert_eq!(find("AnubisProvider"), None, "behavior is not ejectable");
    }

    #[test]
    fn relative_imports_are_the_ones_an_ejection_decides_about() {
        let module = "\
// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Switch } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'
";
        let imports = relative_imports(module);
        assert_eq!(
            imports
                .iter()
                .map(|import| import.specifier.as_str())
                .collect::<Vec<_>>(),
            ["./types", "./FieldWrapper"],
        );
        assert!(imports[0].is_type);
        assert_eq!(imports[0].names, ["AnubisFieldProps"]);
        assert!(!imports[1].is_type);
    }

    #[test]
    fn an_ejected_module_reads_public_dependencies_from_the_package() {
        let module = "\
// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Switch } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

export function BooleanField() {}
";
        let ejected = eject_module(
            module,
            &banner("src/fields/BooleanField.tsx", "0.1.0", "2026-08-15"),
            &[
                "./types".to_owned(),
                "./FieldWrapper".to_owned(),
                "./useFieldState".to_owned(),
            ],
        );

        assert_eq!(
            ejected,
            "\
// Copyright © 2026 Jalapeno Labs

// Ejected from @jalapenolabs/anubis v0.1.0 (src/fields/BooleanField.tsx) on 2026-08-15.
// This file belongs to this application now: upstream improvements to it no longer
// arrive. Delete it to go back to the package's copy.

import type { AnubisFieldProps } from '@jalapenolabs/anubis'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Switch } from '@heroui/react'

// Misc
import { FieldWrapper, useFieldState } from '@jalapenolabs/anubis'

export function BooleanField() {}
",
            "{ejected}",
        );
    }

    /// The heavy fields fetch their editors at runtime, and a copy that left
    /// the editor behind would break the moment the form was opened.
    #[test]
    fn a_lazily_imported_module_is_found_too() {
        let module = "\
// Copyright © 2026 Jalapeno Labs

// Core
import { Suspense, lazy } from 'react'

const RichTextEditor = lazy(async () => {
  const editor = await import('./internal/RichTextEditor')
  return { default: editor.RichTextEditor }
})
";
        let imports = relative_imports(module);
        assert_eq!(
            imports
                .iter()
                .map(|import| import.specifier.as_str())
                .collect::<Vec<_>>(),
            ["./internal/RichTextEditor"],
            "the react import is not relative, and the lazy one is",
        );
        assert!(
            !imports[0].is_public(&root_exports("export { RichTextEditor } from './x'\n")),
            "a module reached only dynamically always rides along",
        );

        // The statement is left exactly where it was: an ejected file fetches
        // the copy beside it, not the package's own.
        let ejected = eject_module(module, "// note\n", &[]);
        assert!(
            ejected.contains("await import('./internal/RichTextEditor')"),
            "{ejected}",
        );
    }

    /// A dependency the package keeps to itself is left relative, because the
    /// ejected tree mirrors the package's layout and rides along with it.
    #[test]
    fn an_internal_dependency_keeps_its_relative_import() {
        let module = "\
// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'

// User interface
import { TextualField } from './internal/TextualField'
";
        let ejected = eject_module(module, "// note\n", &["./types".to_owned()]);
        assert!(ejected.contains("import { TextualField } from './internal/TextualField'"));
        assert!(ejected.contains("import type { AnubisFieldProps } from '@jalapenolabs/anubis'"));
    }

    #[test]
    fn a_shared_import_line_splits_around_the_ejected_name() {
        let consumer = "\
// Copyright © 2026 Jalapeno Labs

import { PasswordField, TextField } from '@jalapenolabs/anubis'

export function DeleteAccountModal() {}
";
        let rewired = rewrite_import(consumer, "TextField", "../anubis/fields/TextField").unwrap();
        assert_eq!(
            rewired,
            "\
// Copyright © 2026 Jalapeno Labs

import { PasswordField } from '@jalapenolabs/anubis'
import { TextField } from '../anubis/fields/TextField'

export function DeleteAccountModal() {}
",
            "{rewired}",
        );
    }

    #[test]
    fn a_wrapped_import_and_a_sole_import_are_both_rewired() {
        let consumer = "\
import {
  isPasskeySupported,
  TextField,
} from '@jalapenolabs/anubis'
import { TextAreaField } from '@jalapenolabs/anubis'
";
        let rewired = rewrite_import(consumer, "TextField", "./anubis/fields/TextField").unwrap();
        assert_eq!(
            rewired,
            "\
import { isPasskeySupported } from '@jalapenolabs/anubis'
import { TextField } from './anubis/fields/TextField'
import { TextAreaField } from '@jalapenolabs/anubis'
",
            "{rewired}",
        );

        let sole = "import { TextField } from '@jalapenolabs/anubis'\n";
        assert_eq!(
            rewrite_import(sole, "TextField", "./anubis/fields/TextField").unwrap(),
            "import { TextField } from './anubis/fields/TextField'\n",
        );
    }

    /// A scaffolded form imports its fields through a list closed by an anchor,
    /// and the anchor has to survive the split: `anubis scaffold field` inserts
    /// the next field's import above it.
    #[test]
    fn a_scaffolders_anchor_stays_in_the_package_import() {
        let form = "\
import {
  TextAreaField,
  TextField,
  // 🐺 anubis:field-imports
} from '@jalapenolabs/anubis'
";
        let rewired = rewrite_import(form, "TextField", "../anubis/fields/TextField").unwrap();
        assert_eq!(
            rewired,
            "\
import {
  TextAreaField,
  // 🐺 anubis:field-imports
} from '@jalapenolabs/anubis'
import { TextField } from '../anubis/fields/TextField'
",
            "{rewired}",
        );

        // The last field ejected leaves the anchor, and the statement that
        // holds it, standing.
        let last = "\
import {
  TextField,
  // 🐺 anubis:field-imports
} from '@jalapenolabs/anubis'
";
        assert_eq!(
            rewrite_import(last, "TextField", "../anubis/fields/TextField").unwrap(),
            "\
import {
  // 🐺 anubis:field-imports
} from '@jalapenolabs/anubis'
import { TextField } from '../anubis/fields/TextField'
",
        );
    }

    /// A comment between the braces may carry an apostrophe, and the specifier
    /// is still the quoted module.
    #[test]
    fn a_comment_in_the_clause_does_not_confuse_the_specifier() {
        let consumer = "\
import {
  TextField,
  // the app's own fields go below
} from '@jalapenolabs/anubis'
";
        let rewired = rewrite_import(consumer, "TextField", "./anubis/fields/TextField").unwrap();
        assert_eq!(
            rewired,
            "\
import {
  // the app's own fields go below
} from '@jalapenolabs/anubis'
import { TextField } from './anubis/fields/TextField'
",
            "{rewired}",
        );
    }

    #[test]
    fn a_renamed_import_moves_under_its_own_name() {
        let consumer = "import { TextField as Text } from '@jalapenolabs/anubis'\n";
        assert_eq!(
            rewrite_import(consumer, "TextField", "./anubis/fields/TextField").unwrap(),
            "import { TextField as Text } from './anubis/fields/TextField'\n",
        );
    }

    #[test]
    fn a_file_that_never_imported_the_component_is_left_alone() {
        let consumer = "import { useCurrentUser } from '@jalapenolabs/anubis'\n";
        assert_eq!(
            rewrite_import(consumer, "TextField", "./anubis/fields/TextField"),
            None,
        );
    }

    /// A merged import too long for one line wraps, as the lint bar requires.
    #[test]
    fn a_long_merged_import_wraps() {
        let module = "\
import { FieldWrapper } from './FieldWrapper'
import { useFieldState } from './useFieldState'
import { SomethingWithAVeryLongNameIndeed } from './somethingWithAVeryLongNameIndeed'
import { AndAnotherLongNameToPushItOver } from './andAnotherLongNameToPushItOver'
";
        let ejected = eject_module(
            module,
            "",
            &[
                "./FieldWrapper".to_owned(),
                "./useFieldState".to_owned(),
                "./somethingWithAVeryLongNameIndeed".to_owned(),
                "./andAnotherLongNameToPushItOver".to_owned(),
            ],
        );
        assert!(
            ejected.contains(
                "\
import {
  AndAnotherLongNameToPushItOver,
  FieldWrapper,
  SomethingWithAVeryLongNameIndeed,
  useFieldState,
} from '@jalapenolabs/anubis'
",
            ),
            "{ejected}",
        );
        assert!(
            ejected.lines().all(|line| line.len() <= MAX_LINE),
            "{ejected}",
        );
    }

    #[test]
    fn the_banner_names_the_version_the_file_came_from() {
        let stamped = banner("src/fields/TextField.tsx", "0.1.0", "2026-08-15");
        assert!(stamped.contains("@jalapenolabs/anubis v0.1.0"));
        assert!(stamped.contains("src/fields/TextField.tsx"));
        assert!(stamped.contains("2026-08-15"));
        assert!(stamped.contains("no longer"));
        assert!(
            stamped.lines().all(|line| line.len() <= MAX_LINE),
            "the banner must fit the lint bar: {stamped}",
        );
    }
}
