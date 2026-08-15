//! Multi-tenancy for Anubis applications.
//!
//! The model follows Bullet Train's, extended one level: users join teams
//! through team memberships, teams belong to organizations, and users also
//! hold organization-level memberships for org-wide roles. Domain resources
//! chain ownership back to a team and are assigned to team memberships, never
//! directly to users. The full design lives in the repository's
//! `docs/tenancy.md`.
//!
//! What a new account joins at signup is a deployment decision:
//! [`BootstrapMode`] gives every account an organization of its own, or puts
//! every account in one shared organization. Applications mount [`router`]
//! (conventionally under `/tenancy`) for the membership overview,
//! invitations, and the management endpoints that create, rename, and
//! dissolve tenants.
//!
//! A deployment with nobody in it yet has nobody to invite the first person
//! either, so [`seed_first_administrator`] opens one from configuration at
//! startup; see [`first_administrator`].

mod bootstrap;
mod departure;
pub mod first_administrator;
mod invitation;
mod management;
mod model;
mod routes;

pub(crate) use bootstrap::{MAX_NAME_CHARS, bootstrap_account};
pub(crate) use departure::settle_departure;

#[doc(inline)]
pub use bootstrap::{ADMIN_ROLE, BootstrapMode};
#[doc(inline)]
pub use first_administrator::seed_first_administrator;
#[doc(inline)]
pub use invitation::{INVITATION_TTL_DAYS, Invitation};
#[doc(inline)]
pub use model::{Organization, OrganizationMembership, Team, TeamMembership};
#[doc(inline)]
pub use routes::router;
