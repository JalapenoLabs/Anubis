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
//! Invitations, the roles.yml compiler, and ownership-chain guards arrive
//! with the following milestone steps.

mod bootstrap;
mod model;

pub(crate) use bootstrap::create_personal_organization;

#[doc(inline)]
pub use bootstrap::ADMIN_ROLE;
#[doc(inline)]
pub use model::{Organization, OrganizationMembership, Team, TeamMembership};
