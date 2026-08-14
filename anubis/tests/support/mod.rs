//! Shared plumbing for the adversarial and concurrency suites.
//!
//! Two pieces, and the split is the point: [`TestDatabase`] owns a Postgres
//! database of the test's own, and [`Harness`] owns the composed application
//! that runs against it. A suite that only needs a database (the migration
//! race, for one) takes the first without the second.
//!
//! Older narratives in this directory share one database and stay out of each
//! other's way by generating random emails, which is enough for a linear story
//! about rows it just wrote. These suites count rows, race writers, and assert
//! on totals, so they take a database each. [`TestDatabase`] documents the
//! adoption path for the rest.

// Each suite compiles this module and uses the part its own story needs, so an
// unused helper here is expected rather than dead.
#![allow(
    dead_code,
    unused_imports,
    reason = "shared by several test binaries, each using a subset"
)]

mod database;
mod harness;
pub mod socket;

pub use database::TestDatabase;
pub use harness::{Harness, PASSWORD, invitation_token, register, send, serve, session_token};
