//! The public REST API: platform credentials and versioned endpoints.
//!
//! Applications mount two routers: [`management::router`] (conventionally at
//! `/developers`) where team admins create platform applications and manage
//! their bearer tokens, and [`v1::router`] at `/api/v1`, the versioned public
//! surface those tokens call. Versioning policy and the overall design live
//! in the repository's `docs/api.md`.

pub mod management;
pub mod v1;

mod platform;
pub(crate) mod typescript;

#[doc(inline)]
pub use platform::PlatformApplication;
