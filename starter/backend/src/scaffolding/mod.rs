//! The living templates every `anubis scaffold model` run transforms.
//!
//! [`absolutely_abstract::CreativeConcept`] is the root (owned by a team),
//! [`completely_concrete::TangibleThing`] is its child, and
//! [`exceedingly_granular::GranularDetail`] is its grandchild. The names come
//! straight from Bullet Train's Super Scaffolding and are deliberate: a
//! two-word `PascalCase` model inside a two-word `snake_case` namespace carries
//! enough shape that the stamping engine can rewrite it into any real-world
//! combination of names without ambiguity.
//!
//! One template per ownership depth, because a depth is a join the generator
//! cannot invent: renaming a two-table query never produces a three-table one.
//! The grandchild is what proves the rule the whole chain rests on, that the
//! team is read off the chain's root in the same query rather than copied onto
//! every row, so a tenant column can never drift.
//!
//! These are not toys. They are ordinary application code, compiled and
//! integration-tested in CI, which is what stops the templates from rotting.
//! Every file here is a complete vertical slice of what one scaffold produces:
//! migration, schema, model with its `valid_*` scoping methods, account
//! handlers, permissions, routes, and tests.
//!
//! [`incidentally_linked::IncidentalLinkage`] is the fourth template, the join
//! model `anubis scaffold join` transforms. A join links two team-owned models,
//! so it needs a second one beside the root: [`merely_peripheral::PeripheralNotion`]
//! is that model, reduced to what a join's far side needs, because a real
//! application's far side always comes from its own `scaffold model` run.
//!
//! [`hypothetically_remote::HypotheticalSenderWebhook`] is the last, and the
//! odd one out: `anubis scaffold webhook` transforms it into a receiver for a
//! third party's events. It is owned by nobody, reached without a session, and
//! stored before it is understood, which is why it shares none of the others'
//! shape.

pub mod absolutely_abstract;
pub mod completely_concrete;
pub mod exceedingly_granular;
pub mod hypothetically_remote;
pub mod incidentally_linked;
pub mod merely_peripheral;
