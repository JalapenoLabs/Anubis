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
//!
//! **Roles are data.** Each organization holds its own roles: named,
//! coloured, ordered bundles of permission keys from the vocabulary
//! `roles.yml` compiles (see [`crate::roles`]), seeded from the roles that file
//! recommends and edited from there. A role is granted through a membership,
//! and the membership is the grant's scope: the organization, a sub-tenant, a
//! team, or one record of a team. [`resolve_organization_access`],
//! [`resolve_sub_tenant_access`], and [`resolve_team_access`] are the one place
//! that decides who reaches what and with which permissions; nothing
//! re-derives them.
//!
//! Every user gets a personal organization with a default sub-tenant and team
//! at signup, so an application that never surfaces the tier still has a
//! complete ownership chain. Applications mount [`router`] (conventionally
//! under `/tenancy`) for the membership overview, invitations, and the
//! management endpoints that create, rename, and dissolve tenants.

mod access;
mod bootstrap;
mod departure;
mod grants;
mod hierarchy;
mod invitation;
mod management;
mod model;
mod role;
mod role_routes;
mod routes;

pub(crate) use access::admitted_teams;
pub(crate) use bootstrap::create_personal_organization;
pub(crate) use departure::settle_departure;
pub(crate) use grants::{organization_administrators, team_administrators};

#[doc(inline)]
pub use crate::roles::{ADMIN_ROLE, DEFAULT_ROLE};
#[doc(inline)]
pub use access::{
    Access, AccessView, Reach, SubTenantAccess, resolve_organization_access,
    resolve_sub_tenant_access, resolve_team_access,
};
#[doc(inline)]
pub use invitation::{INVITATION_TTL_DAYS, Invitation};
#[doc(inline)]
pub use model::{
    Organization, OrganizationAccess, OrganizationMembership, SubTenant, SubTenantMembership, Team,
    TeamMembership,
};
#[doc(inline)]
pub use role::{Role, RoleSummary, RoleView};
#[doc(inline)]
pub use routes::router;
