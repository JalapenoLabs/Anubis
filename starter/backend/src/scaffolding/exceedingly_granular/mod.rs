//! The `GranularDetail` slice: model, routes, and permissions key.

mod model;
mod routes;

pub use model::{GranularDetail, MODEL};
pub use routes::{api_router, openapi, router};
