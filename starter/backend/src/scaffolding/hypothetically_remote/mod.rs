//! The `HypotheticalSenderWebhook` slice: model, endpoint, and processing job.
//!
//! This is the living template `anubis scaffold webhook <Provider>` transforms
//! into a receiver for a real provider. It is a whole vertical slice of one:
//! the table a received request is stored in, the unauthenticated endpoint that
//! stores it, the signature check the developer finishes, and the background
//! job that processes it.

mod job;
mod model;
mod routes;

pub use job::{ProcessHypotheticalSenderWebhook, register_jobs};
pub use model::HypotheticalSenderWebhook;
pub use routes::{SIGNATURE_HEADER, SIGNING_SECRET_VAR, router, verify_signature};
