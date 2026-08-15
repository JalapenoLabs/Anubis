//! Request extractors for authenticated handlers.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;

use crate::auth::model::User;
use crate::auth::{account_status, session};
use crate::db::DbPool;
use crate::http::ApiError;

/// Extracts the signed-in user from the session cookie, or rejects with 401.
///
/// Handlers take `CurrentUser` as an argument to require authentication:
///
/// ```ignore
/// async fn profile(CurrentUser(user): CurrentUser) -> Json<UserResponse> { ... }
/// ```
///
/// It also enforces the account's own state: a disabled account and one owing
/// a password change are refused with `403` here, before any handler body
/// runs, so a route added later cannot forget to ask. See
/// [`crate::auth::account_status`] for the codes those refusals carry.
///
/// The extractor reads the connection pool from request extensions, which the
/// auth router provides for its own routes. Application routers using
/// `CurrentUser` add `.layer(Extension(pool.clone()))` the same way.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub User);

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user = signed_in_user(parts).await?;
        account_status::require_usable(&user)?;
        Ok(Self(user))
    }
}

/// The signed-in user at the one route the rotation demand exempts.
///
/// A flagged account has exactly one thing it may do, and it is this: refusing
/// the password change would leave nothing that clears the flag. The type is
/// crate-private on purpose, so the exemption is a fact about the framework's
/// own route rather than an opt-out an application can reach for. Sign-out is
/// the other exemption and needs no extractor at all, since it reads the
/// cookie directly.
///
/// A disabled account is still refused: it may not act at all.
#[derive(Debug, Clone)]
pub(crate) struct RotatingUser(pub User);

impl<S> FromRequestParts<S> for RotatingUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user = signed_in_user(parts).await?;
        account_status::require_enabled(&user)?;
        Ok(Self(user))
    }
}

/// Resolves the session cookie to its user, or rejects with 401.
async fn signed_in_user(parts: &Parts) -> Result<User, ApiError> {
    let Some(pool) = parts.extensions.get::<DbPool>().cloned() else {
        tracing::error!(
            "CurrentUser used on a router without a DbPool extension; \
             add .layer(Extension(pool.clone())) to the router",
        );
        return Err(ApiError::internal());
    };

    let jar = CookieJar::from_headers(&parts.headers);
    let Some(cookie) = jar.get(session::SESSION_COOKIE) else {
        return Err(not_signed_in());
    };

    let mut connection = pool.get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "session lookup failed: {{error.message}}",
        );
        ApiError::internal()
    })?;

    let user = session::find_user(&mut connection, cookie.value())
        .await
        .map_err(|error| {
            tracing::error!(
                error.message = %error,
                "session lookup failed: {{error.message}}",
            );
            ApiError::internal()
        })?;

    user.ok_or_else(not_signed_in)
}

fn not_signed_in() -> ApiError {
    ApiError::unauthorized("You are not signed in.")
}
