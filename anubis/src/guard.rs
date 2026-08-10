//! Ownership-chain authorization guards.
//!
//! Handlers opt into tenancy enforcement by taking a guard extractor as an
//! argument. [`TeamMember`] resolves the route's `{team_id}` against the
//! signed-in user's membership; [`OrganizationMember`] does the same for
//! `{organization_id}`. Extraction rejects before the handler body runs:
//! `401` when not signed in, `404` when the target does not exist *or* the
//! user is not a member (identical responses, so probing ids reveals
//! nothing).
//!
//! Permissions come from the compiled role set: `member.require(action,
//! model)` answers `403` unless one of the membership's roles grants the
//! action. Scaffolded models resolve their parent chain to the owning team
//! and ride the same primitives.
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
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use tower::layer::util::{Identity, Stack};
use uuid::Uuid;

use crate::auth::{CurrentUser, User};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::roles::{Action, RoleSet};
use crate::schema::{organization_memberships, organizations, team_memberships, teams};
use crate::tenancy::{Organization, OrganizationMembership, Team, TeamMembership};

/// Bundles the request extensions the guard extractors need.
///
/// Add to any router whose handlers use [`TeamMember`], [`OrganizationMember`],
/// or [`CurrentUser`].
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

/// The signed-in user's standing in the route's `{team_id}` team.
#[derive(Debug, Clone)]
pub struct TeamMember {
    /// The signed-in user.
    pub user: User,
    /// The team named in the route.
    pub team: Team,
    /// The user's membership in that team.
    pub membership: TeamMembership,
    roles: Arc<RoleSet>,
}

impl TeamMember {
    /// Returns `true` when a held role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.roles.can(&self.membership.roles, action, model)
    }

    /// Rejects with `403` unless a held role grants the action on the model.
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

        let found: Option<(Team, TeamMembership)> = teams::table
            .inner_join(
                team_memberships::table.on(team_memberships::team_id
                    .eq(teams::id)
                    .and(team_memberships::user_id.eq(user.id))),
            )
            .filter(teams::id.eq(team_id))
            .select((Team::as_select(), TeamMembership::as_select()))
            .first(&mut connection)
            .await
            .optional()
            .map_err(log_internal)?;

        // Unknown team and non-membership answer identically.
        let (team, membership) = found.ok_or_else(ApiError::not_found)?;

        Ok(Self {
            user,
            team,
            membership,
            roles,
        })
    }
}

/// The signed-in user's standing in the route's `{organization_id}` organization.
#[derive(Debug, Clone)]
pub struct OrganizationMember {
    /// The signed-in user.
    pub user: User,
    /// The organization named in the route.
    pub organization: Organization,
    /// The user's membership in that organization.
    pub membership: OrganizationMembership,
    roles: Arc<RoleSet>,
}

impl OrganizationMember {
    /// Returns `true` when a held role grants the action on the model.
    #[must_use]
    pub fn can(&self, action: Action, model: &str) -> bool {
        self.roles.can(&self.membership.roles, action, model)
    }

    /// Rejects with `403` unless a held role grants the action on the model.
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

        let found: Option<(Organization, OrganizationMembership)> = organizations::table
            .inner_join(
                organization_memberships::table.on(organization_memberships::organization_id
                    .eq(organizations::id)
                    .and(organization_memberships::user_id.eq(user.id))),
            )
            .filter(organizations::id.eq(organization_id))
            .select((
                Organization::as_select(),
                OrganizationMembership::as_select(),
            ))
            .first(&mut connection)
            .await
            .optional()
            .map_err(log_internal)?;

        // Unknown organization and non-membership answer identically.
        let (organization, membership) = found.ok_or_else(ApiError::not_found)?;

        Ok(Self {
            user,
            organization,
            membership,
            roles,
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
