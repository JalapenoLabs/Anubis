//! The `CreativeConcept` slice: model, routes, and permissions key.

mod model;
mod routes;

pub use model::{CreativeConcept, MODEL};
pub use routes::{api_router, openapi, router};
