//! The `TangibleThing` slice: the child template, owned through its parent.

mod model;
mod routes;

// The child model type stays internal: nothing outside this slice names it
// yet. The parent's does not, because this slice scopes against it.
pub use routes::router;
