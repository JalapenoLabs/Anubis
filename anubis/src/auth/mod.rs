//! Authentication for Anubis applications.
//!
//! Email/password authentication with argon2id hashing and Postgres-backed
//! cookie sessions. Applications mount [`router`] (conventionally under
//! `/auth`) for registration, login, logout, and current-user endpoints, and
//! guard their own handlers with the [`CurrentUser`] extractor. Sign-in also
//! covers emailed codes, passkeys, TOTP as a second factor, and OAuth through
//! OpenID Connect; the design lives in the repository's `docs/api.md`.
//!
//! An account also carries a state an administrator controls: it can be
//! disabled, and it can be made to change its password before it does
//! anything else. Both are enforced by the extractor and by every path that
//! mints a session; see [`account_status`].

pub mod account_status;
pub mod oauth;
pub mod password;
pub mod registration;
pub mod secret_box;
pub mod totp;

mod account;
mod avatar;
mod email_code;
mod extract;
mod mfa;
mod model;
mod passkey;
pub(crate) mod routes;
mod session;
pub(crate) mod token;
mod user_token;

#[doc(inline)]
pub use avatar::public_router as avatar_router;
#[doc(inline)]
pub use extract::CurrentUser;
#[doc(inline)]
pub use model::{User, UserResponse};
#[doc(inline)]
pub use registration::RegistrationMode;
#[doc(inline)]
pub use routes::router;
#[doc(inline)]
pub use session::{SESSION_COOKIE, SESSION_TTL_DAYS};
