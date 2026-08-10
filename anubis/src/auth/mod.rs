//! Authentication for Anubis applications.
//!
//! Email/password authentication with argon2id hashing and Postgres-backed
//! cookie sessions. Applications mount [`router`] (conventionally under
//! `/auth`) for registration, login, logout, and current-user endpoints, and
//! guard their own handlers with the [`CurrentUser`] extractor. Email
//! verification and OAuth arrive with later milestone steps; the design lives
//! in the repository's `docs/api.md`.

pub mod password;

mod extract;
mod model;
mod routes;
mod session;
pub(crate) mod token;
mod user_token;

#[doc(inline)]
pub use extract::CurrentUser;
#[doc(inline)]
pub use model::{User, UserResponse};
#[doc(inline)]
pub use routes::router;
#[doc(inline)]
pub use session::{SESSION_COOKIE, SESSION_TTL_DAYS};
