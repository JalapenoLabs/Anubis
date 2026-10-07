//! Request extractors for authenticated handlers.
//!
//! [`CurrentUser`] is the extractor every authenticated handler takes, and it
//! is where a temporary password is enforced: an account an operator gave one
//! is refused with `403` and the code [`PASSWORD_CHANGE_REQUIRED`] until it
//! chooses its own. Putting the rule in the extractor rather than in handlers
//! is what makes it hold for routes nobody has written yet, the application's
//! included, and for every guard built on it ([`crate::guard::PlatformMember`]
//! and the tenant guards all resolve the user through it).
//!
//! [`SignedIn`] is the narrow exception, taken only by the routes an account
//! must reach to get out of that state: reading itself and changing its
//! password. Signing out reads the cookie directly and needs neither.
//!
//! A connected client is held to the same rule: the resource side's
//! [`crate::oauth_server::Bearer`] refuses a flagged account through
//! [`refuse_temporary_password`], so a program the person connected before the
//! rescue cannot act for them while the account is still on the operator's
//! password.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;

use crate::auth::model::User;
use crate::auth::session;
use crate::db::DbPool;
use crate::http::ApiError;

/// The code a `403` carries when the account must change its password first.
///
/// Stable, because a client branches on it: the SPA answers it by sending the
/// person to the change-password screen rather than showing the message.
pub const PASSWORD_CHANGE_REQUIRED: &str = "password_change_required";

/// Extracts the signed-in user from the session cookie, or rejects.
///
/// Handlers take `CurrentUser` as an argument to require authentication:
///
/// ```ignore
/// async fn profile(CurrentUser(user): CurrentUser) -> Json<UserResponse> { ... }
/// ```
///
/// Rejects with `401` when nobody is signed in, and with `403` carrying the
/// code [`PASSWORD_CHANGE_REQUIRED`] when the account is signed in on a
/// temporary password; see the module docs.
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
        refuse_temporary_password(&user)?;
        Ok(Self(user))
    }
}

/// Refuses an account that must replace its temporary password first.
///
/// The one statement of the rule, shared by every credential that resolves to
/// an account: the session's [`CurrentUser`] and the connected client's
/// [`crate::oauth_server::Bearer`]. Both answer the same `403` and the same
/// code, so a client branches on one value whichever way it signed in.
///
/// # Errors
/// Returns a `403` carrying [`PASSWORD_CHANGE_REQUIRED`] when the account is
/// on a temporary password.
pub(crate) fn refuse_temporary_password(user: &User) -> Result<(), ApiError> {
    if !user.password_change_required {
        return Ok(());
    }

    tracing::debug!(
        user.id = %user.id,
        "refused {{user.id}} until it replaces its temporary password",
    );
    Err(
        ApiError::forbidden("Choose a new password before you continue.")
            .with_code(PASSWORD_CHANGE_REQUIRED),
    )
}

/// Extracts the signed-in user, admitting one that must change its password.
///
/// Only the routes an account needs while it holds a temporary password take
/// this: `GET /auth/me`, so the SPA can see the flag, and `POST
/// /auth/change-password`, which clears it. Everything else takes
/// [`CurrentUser`], and a handler reaching for this one is opting out of the
/// rule on purpose.
#[derive(Debug, Clone)]
pub struct SignedIn(pub User);

impl<S> FromRequestParts<S> for SignedIn
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        signed_in_user(parts).await.map(Self)
    }
}

/// Resolves the session cookie to its account, or rejects with `401`.
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
