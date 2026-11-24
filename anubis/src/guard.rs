//! Ownership-chain authorization guards.
//!
//! Handlers opt into tenancy enforcement by taking a guard extractor as an
//! argument. [`TeamMember`] resolves the route's `{team_id}` against the
//! signed-in user's membership; [`OrganizationMember`] does the same for
//! `{organization_id}`, and [`SubTenantMember`] for `{sub_tenant_id}`.
//! Extraction rejects before the handler body runs: `401` when not signed in,
//! `404` when the target does not exist *or* the user is not a member
//! (identical responses, so probing ids reveals nothing).
//!
//! [`PlatformMember`] is the same idea one tier further out. The platform is
//! the deployment, it names no id in the route and has no roster, so the guard
//! admits the accounts holding a role `roles.yml` scoped to `platform` and
//! answers every other account the same `404`.
//!
//! Every guard honors a suspension, which is a deny rather than a missing
//! grant: a suspended organization membership closes every tenant in the
//! organization, and a suspended sub-tenant membership closes that sub-tenant
//! and the teams scoped to it. The rule and its reasoning live with
//! [`anubis::tenancy::resolve_sub_tenant_access`], which is the one place that
//! resolves sub-tenant access.
//!
//! [`anubis::tenancy::resolve_sub_tenant_access`]: crate::tenancy::resolve_sub_tenant_access
//!
//! Permissions come from the organization's runtime roles, drawn from the
//! vocabulary `roles.yml` compiles: `member.require(action, model)` answers
//! `403` unless a role that applies where the member stands grants the action.
//! The tenant guards are thin: each asks one resolver in [`crate::tenancy`],
//! the one place that decides reach and roles, and carries its answer.
//! Scaffolded models resolve their parent chain to the owning team and ride
//! the same resolver, narrowed to the records in hand.
//!
//! Routers using these extractors provide two request extensions, which
//! [`layer`] bundles:
//!
//! ```ignore
//! let app = Router::new()
//!     .route("/teams/{team_id}/projects", get(list_projects))
//!     .layer(anubis::guard::layer(pool, role_set));
//!
//! async fn list_projects(member: TeamMember) -> Result<..., ApiError> {
//!     member.require(Action::Read, "Project")?;
//!     ...
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::{Extension, RequestPartsExt};
use tower::layer::util::{Identity, Stack};
use uuid::Uuid;

use crate::auth::session::Sessions;
use crate::auth::{CurrentUser, User};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::roles::{Action, RoleSet, Scope};
use crate::tenancy::{
    Access, Organization, OrganizationMembership, SubTenant, SubTenantAccess, Team, TeamMembership,
    resolve_organization_access, resolve_sub_tenant_access, resolve_team_access,
};

/// Bundles the request extensions the guard extractors need.
///
/// Add to any router whose handlers use [`TeamMember`], [`SubTenantMember`],
/// [`OrganizationMember`], or [`CurrentUser`].
#[must_use]
pub fn layer(
    pool: DbPool,
    roles: RoleSet,
) -> Stack<Extension<DbPool>, Stack<Extension<Arc<RoleSet>>, Identity>> {
    tower::ServiceBuilder::new()
        .layer(Extension(Arc::new(roles)))
        .layer(Extension(pool))
        .into_inner()
}

/// A signed-in user who operates the deployment itself.
///
/// The platform is the tier above every organization, and it has no roster to
/// join through, so the grant lives on the account: a user holding any role
/// key `roles.yml` scoped to `platform` extracts, and everybody else is
/// answered `404`, byte-identical to a route that does not exist. That is the
/// same opacity the tenant guards give an id the caller cannot reach, and here
/// it keeps the operator surface from announcing itself to the accounts it
/// refuses.
///
/// Holding a platform role is admission, not permission: what an operator may
/// do still comes from the compiled role set, so a handler asks for the action
/// it is about to take.
///
/// ```ignore
/// async fn suspend(operator: PlatformMember) -> Result<..., ApiError> {
///     operator.require(Action::Update, "Account")?;
///     ...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PlatformMember {
    /// The signed-in operator.
    pub user: User,
    roles: Arc<RoleSet>,
    sessions: Sessions,
}

impl PlatformMember {
    /// The compiled role set the guard admitted this operator against.
    ///
    /// Read by the platform primitives that grant a role, so the role an
    /// operator hands out is checked against the same file that admitted them.
    pub(crate) fn roles(&self) -> &RoleSet {
        &self.roles
    }

    /// The session store the operator's own session was found in.
    ///
    /// Read by the platform primitives that sign an account out, which run
    /// with no request of their own to find the store on, so the one this
    /// request carries is the one they use.
    pub(crate) fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    /// Returns `true` when a held platform role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.roles.can(&self.user.platform_roles, action, model)
    }

    /// Rejects with `403` unless a held platform role grants the action.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, action: Action, model: &str) -> Result<(), ApiError> {
        if self.can(action, model) {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "You do not have permission to do that.",
            ))
        }
    }
}

impl<S> FromRequestParts<S> for PlatformMember
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let (_pool, roles) = guard_extensions(parts)?;

        // A key the application later removed from roles.yml, or scoped away
        // from the platform, stops admitting its holders the moment the file
        // says so: the column is a grant, and the file is what a grant means.
        let operates = user
            .platform_roles
            .iter()
            .any(|role| roles.is_grantable_at(role, Scope::Platform));
        if !operates {
            return Err(ApiError::not_found());
        }

        Ok(Self {
            user,
            roles,
            sessions: Sessions::for_request(parts),
        })
    }
}

/// The signed-in user's standing in the route's `{team_id}` team.
///
/// The membership admits the caller and the [`Access`] says what they may
/// do: the team-wide roles granted through the membership, and the default
/// role everybody holds, resolved by
/// [`anubis::tenancy::resolve_team_access`](crate::tenancy::resolve_team_access).
#[derive(Debug, Clone)]
pub struct TeamMember {
    /// The signed-in user.
    pub user: User,
    /// The team named in the route.
    pub team: Team,
    /// The user's membership in that team.
    pub membership: TeamMembership,
    /// What the roles that apply in the team allow.
    pub access: Access,
}

impl TeamMember {
    /// Returns `true` when a held role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.access.can(action, model)
    }

    /// Rejects with `403` unless a held role grants the action on the model.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, action: Action, model: &str) -> Result<(), ApiError> {
        self.access.require(action, model)
    }
}

impl<S> FromRequestParts<S> for TeamMember
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let team_id = path_uuid(parts, "team_id").await?;
        let (pool, roles) = guard_extensions(parts)?;

        let mut connection = pool.get().await.map_err(log_internal)?;

        let found = resolve_team_access(&mut connection, &roles, user.id, team_id, &[])
            .await
            .map_err(log_internal)?;

        // Unknown team, non-membership, and suspension answer identically.
        let (team, membership, access) = found.ok_or_else(ApiError::not_found)?;

        Ok(Self {
            user,
            team,
            membership,
            access,
        })
    }
}

/// The signed-in user's standing in the route's `{sub_tenant_id}` sub-tenant.
///
/// A caller reaches a sub-tenant along four paths (organization administrator,
/// full organization member, an explicit grant, or a team that reaches it),
/// and this guard never works out which for itself: it asks
/// [`resolve_sub_tenant_access`], the one function that answers, and the
/// [`access`](Self::access) it carries is that answer.
#[derive(Debug, Clone)]
pub struct SubTenantMember {
    /// The signed-in user.
    pub user: User,
    /// The sub-tenant named in the route.
    pub sub_tenant: SubTenant,
    /// How the user reaches it, and what the roles that apply there allow.
    pub access: SubTenantAccess,
}

impl SubTenantMember {
    /// Returns `true` when a held role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.access.access.can(action, model)
    }

    /// Rejects with `403` unless a held role grants the action on the model.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, action: Action, model: &str) -> Result<(), ApiError> {
        self.access.access.require(action, model)
    }
}

impl<S> FromRequestParts<S> for SubTenantMember
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let sub_tenant_id = path_uuid(parts, "sub_tenant_id").await?;
        let (pool, roles) = guard_extensions(parts)?;

        let mut connection = pool.get().await.map_err(log_internal)?;

        let found = resolve_sub_tenant_access(&mut connection, &roles, user.id, sub_tenant_id)
            .await
            .map_err(log_internal)?;

        // Unknown sub-tenant and unreachable sub-tenant answer identically.
        let (sub_tenant, access) = found.ok_or_else(ApiError::not_found)?;

        Ok(Self {
            user,
            sub_tenant,
            access,
        })
    }
}

/// The signed-in user's standing in the route's `{organization_id}` organization.
///
/// The [`Access`] holds the organization-wide roles granted through the
/// membership and the default role, resolved by
/// [`anubis::tenancy::resolve_organization_access`](crate::tenancy::resolve_organization_access).
#[derive(Debug, Clone)]
pub struct OrganizationMember {
    /// The signed-in user.
    pub user: User,
    /// The organization named in the route.
    pub organization: Organization,
    /// The user's membership in that organization.
    pub membership: OrganizationMembership,
    /// What the roles that apply organization-wide allow.
    pub access: Access,
}

impl OrganizationMember {
    /// Returns `true` when a held role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.access.can(action, model)
    }

    /// Rejects with `403` unless a held role grants the action on the model.
    ///
    /// # Errors
    /// Returns a forbidden [`ApiError`] when no held role grants the action.
    pub fn require(&self, action: Action, model: &str) -> Result<(), ApiError> {
        self.access.require(action, model)
    }
}

impl<S> FromRequestParts<S> for OrganizationMember
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let organization_id = path_uuid(parts, "organization_id").await?;
        let (pool, roles) = guard_extensions(parts)?;

        let mut connection = pool.get().await.map_err(log_internal)?;

        let found = resolve_organization_access(&mut connection, &roles, user.id, organization_id)
            .await
            .map_err(log_internal)?;

        // Unknown organization, non-membership, and suspension answer
        // identically.
        let (organization, membership, access) = found.ok_or_else(ApiError::not_found)?;

        Ok(Self {
            user,
            organization,
            membership,
            access,
        })
    }
}

/// Reads a UUID path parameter, answering `404` for malformed ids.
async fn path_uuid(parts: &mut Parts, name: &str) -> Result<Uuid, ApiError> {
    let Ok(Path(params)) = parts.extract::<Path<HashMap<String, String>>>().await else {
        tracing::error!(
            url.path.param = name,
            "guard used on a route without path parameters; \
             add {{{{url.path.param}}}} to the route path",
        );
        return Err(ApiError::internal());
    };

    let Some(raw) = params.get(name) else {
        tracing::error!(
            url.path.param = name,
            "guard used on a route missing the {{url.path.param}} path parameter",
        );
        return Err(ApiError::internal());
    };

    // A malformed id can never exist, and existence is not revealed.
    raw.parse().map_err(|_error| ApiError::not_found())
}

fn guard_extensions(parts: &Parts) -> Result<(DbPool, Arc<RoleSet>), ApiError> {
    let Some(pool) = parts.extensions.get::<DbPool>().cloned() else {
        tracing::error!(
            "guard used on a router without a DbPool extension; \
             add anubis::guard::layer(pool, roles) to the router",
        );
        return Err(ApiError::internal());
    };
    let Some(roles) = parts.extensions.get::<Arc<RoleSet>>().cloned() else {
        tracing::error!(
            "guard used on a router without a RoleSet extension; \
             add anubis::guard::layer(pool, roles) to the router",
        );
        return Err(ApiError::internal());
    };
    Ok((pool, roles))
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "guard lookup failed: {{error.message}}",
    );
    ApiError::internal()
}
