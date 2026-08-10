//! The template-stamping engine behind every `anubis` scaffolder.
//!
//! Anubis scaffolding follows Bullet Train's living-templates philosophy:
//! templates are real, compiling code, and generators are text transforms
//! over them. This module is the pure core of that machinery, shared by
//! `anubis new` and the `anubis scaffold` family:
//!
//! - [`Names`] and [`pluralize`] derive every casing and plural variant of a
//!   model name from one input.
//! - [`Replacements`] rewrites template identifiers into target identifiers,
//!   longest pattern first.
//! - [`insert_above_anchor`] inserts generated lines above a magic anchor
//!   comment in a file the application owns, idempotently. The [`anchor`]
//!   module names every anchor the framework recognizes.
//! - [`table_block`] and [`line_containing`] read declarations back out of the
//!   application's own files, which is how a generated table inherits the
//!   template's shape rather than a shape hard-coded here.
//! - [`Field`] and [`FieldType`] map a `name:type` argument to a column, and
//!   [`ModelScaffold`] plans a whole `scaffold model` run from its arguments.
//!
//! Everything here is pure string-to-string transformation. File discovery,
//! reading, and writing belong to the CLI, which keeps this engine trivially
//! unit-testable.
//!
//! ```
//! use anubis::scaffold::{Names, Replacements};
//!
//! let template = Names::parse("TangibleThing").unwrap();
//! let target = Names::parse("Invoice").unwrap();
//! let replacements = Replacements::between(&template, &target);
//! assert_eq!(replacements.apply("list_tangible_things()"), "list_invoices()");
//! ```

pub mod anchor;

mod error;
mod extract;
mod field;
mod inflect;
mod model;
mod stamp;

#[doc(inline)]
pub use error::ScaffoldError;
#[doc(inline)]
pub use extract::{line_containing, table_block};
#[doc(inline)]
pub use field::{FIELD_TYPES, Field, FieldType};
#[doc(inline)]
pub use inflect::{NameError, Names, pluralize};
#[doc(inline)]
pub use model::{ModelScaffold, ModelTemplate};
#[doc(inline)]
pub use stamp::{AnchorError, Replacements, insert_above_anchor};
