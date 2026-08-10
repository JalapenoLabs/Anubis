//! Request extractors for authenticated handlers.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;

use crate::auth::model::User;
use crate::auth::session;
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

        user.map(CurrentUser).ok_or_else(not_signed_in)
    }
}

fn not_signed_in() -> ApiError {
    ApiError::unauthorized("You are not signed in.")
}
