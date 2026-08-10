//! Multi-tenancy for Anubis applications.
//!
//! The model follows Bullet Train's, extended one level: users join teams
//! through team memberships, teams belong to organizations, and users also
//! hold organization-level memberships for org-wide roles. Domain resources
//! chain ownership back to a team and are assigned to team memberships, never
//! directly to users. The full design lives in the repository's
//! `docs/tenancy.md`.
//!
//! Every user gets a personal organization with a default team at signup.
//! Applications mount [`router`] (conventionally under `/tenancy`) for
//! invitations; ownership-chain guards arrive with the next milestone step.

mod bootstrap;
mod invitation;
mod model;
mod routes;

pub(crate) use bootstrap::create_personal_organization;

#[doc(inline)]
pub use bootstrap::ADMIN_ROLE;
#[doc(inline)]
pub use invitation::{INVITATION_TTL_DAYS, Invitation};
#[doc(inline)]
pub use model::{Organization, OrganizationMembership, Team, TeamMembership};
#[doc(inline)]
pub use routes::router;
