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
//! - [`insert_json_entries`] does the same for a locale file, which is JSON
//!   and cannot hold a comment, by finding the model's own object.
//! - [`table_block`] and [`line_containing`] read declarations back out of the
//!   application's own files, which is how a generated table inherits the
//!   template's shape rather than a shape hard-coded here.
//! - [`Field`] and [`FieldType`] map a `name:type` argument to a column, or to
//!   an [`Association`] when it names one, [`FieldScaffold`] turns one field
//!   into every line it contributes to each [`Artifact`], and [`ModelScaffold`]
//!   plans a whole `scaffold model` run.
//! - [`JoinScaffold`] plans a `scaffold join` run: the join table, model, and
//!   endpoints two existing models need before an association can reach them.
//! - [`OauthScaffold`] plans a `scaffold oauth` run, which writes no file: a
//!   provider is enabled by its credentials and the sign-in page renders the
//!   providers the backend reports, so all it computes is the redirect URI
//!   that provider's console needs.
//! - [`WebhookScaffold`] plans a `scaffold webhook` run: the table, endpoint,
//!   and processing job one third party's events are received through.
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
mod join;
mod model;
mod oauth;
mod stamp;
mod webhook;

#[doc(inline)]
pub use error::ScaffoldError;
#[doc(inline)]
pub use extract::{line_containing, table_block};
#[doc(inline)]
pub use field::{
    Artifact, Association, BelongsTo, FIELD_TYPES, Field, FieldScaffold, FieldType, LOCALE_FIELDS,
    Source, TEAM_MEMBERSHIP,
};
#[doc(inline)]
pub use inflect::{NameError, Names, pluralize};
#[doc(inline)]
pub use join::{JoinScaffold, JoinTemplate};
#[doc(inline)]
pub use model::{ChildAttachment, ModelScaffold, ModelTemplate, locale_file, model_artifacts};
#[doc(inline)]
pub use oauth::OauthScaffold;
#[doc(inline)]
pub use stamp::{AnchorError, Replacements, insert_above_anchor, insert_json_entries};
#[doc(inline)]
pub use webhook::{WebhookScaffold, WebhookTemplate};
