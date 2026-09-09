//! Multi-tenancy for Anubis applications.
//!
//! The model follows Bullet Train's, extended two levels: users join teams
//! through team memberships, teams belong to organizations, and users also
//! hold organization-level memberships for org-wide roles. Domain resources
//! chain ownership back to a team and are assigned to team memberships, never
//! directly to users. The full design lives in the repository's
//! `docs/tenancy.md`.
//!
//! Between the two sits the **sub-tenant**, the tier that owns the work: what
//! GCP calls a project and GitHub calls a repository. It holds its own
//! membership and its own teams, and a team either scopes itself to one
//! sub-tenant or stays organization-level and is inherited by all of them.
//! [`resolve_sub_tenant_access`] is the one function that decides who reaches
//! a sub-tenant and with which roles; nothing re-derives it.
//!
//! Every user gets a personal organization with a default sub-tenant and team
//! at signup, so an application that never surfaces the tier still has a
//! complete ownership chain. Applications mount [`router`] (conventionally
//! under `/tenancy`) for the membership overview, invitations, and the
//! management endpoints that create, rename, and dissolve tenants.

mod access;
mod bootstrap;
mod departure;
mod invitation;
mod management;
mod model;
mod routes;

pub(crate) use bootstrap::create_personal_organization;
pub(crate) use departure::settle_departure;

#[doc(inline)]
pub use access::{Reach, SubTenantAccess, resolve_sub_tenant_access};
#[doc(inline)]
pub use bootstrap::ADMIN_ROLE;
#[doc(inline)]
pub use invitation::{INVITATION_TTL_DAYS, Invitation};
#[doc(inline)]
pub use model::{
    Organization, OrganizationAccess, OrganizationMembership, SubTenant, SubTenantMembership, Team,
    TeamMembership,
};
#[doc(inline)]
pub use routes::router;
