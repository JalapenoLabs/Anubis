//! The living templates every `anubis scaffold model` run transforms.
//!
//! [`absolutely_abstract::CreativeConcept`] is the parent (owned by a team)
//! and [`completely_concrete::TangibleThing`] is its child. The names come
//! straight from Bullet Train's Super Scaffolding and are deliberate: a
//! two-word `PascalCase` model inside a two-word `snake_case` namespace carries
//! enough shape that the stamping engine can rewrite it into any real-world
//! combination of names without ambiguity.
//!
//! These are not toys. They are ordinary application code, compiled and
//! integration-tested in CI, which is what stops the templates from rotting.
//! Every file here is a complete vertical slice of what one scaffold produces:
//! migration, schema, model with its `valid_*` scoping methods, account
//! handlers, permissions, routes, and tests.

pub mod absolutely_abstract;
pub mod completely_concrete;
