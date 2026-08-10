//! Authentication for Anubis applications.
//!
//! Email/password authentication with argon2id hashing. Applications mount
//! [`router`] (conventionally under `/auth`) to get registration and login
//! endpoints backed by the framework's `users` table. Sessions, email
//! verification, and OAuth arrive with later milestone steps; the design
//! lives in the repository's `docs/api.md`.

pub mod password;

mod model;
mod routes;

#[doc(inline)]
pub use model::{User, UserResponse};
#[doc(inline)]
pub use routes::router;
