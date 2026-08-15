//! Disabled accounts and forced password rotation.
//!
//! Two columns on `users` carry everything an administrator can say about an
//! account without deleting it:
//!
//! - `disabled_at` is offboarding that keeps the rows the person authored. A
//!   disabled account authenticates against nothing: [`require_enabled_to_sign_in`]
//!   refuses every path that would mint a session, and [`disable`] deletes the
//!   sessions the account already had rather than letting them run out the
//!   clock.
//! - `password_change_required` is a credential an administrator provisioned
//!   and wants replaced. The account may sign in and may change its password;
//!   every other authenticated route refuses until it does.
//!
//! Enforcement lives in the [`CurrentUser`](crate::auth::CurrentUser)
//! extractor rather than in each handler, so a route added later cannot
//! forget it, and in [`signed_in_jar`](crate::auth::routes::signed_in_jar),
//! which every sign-in path funnels through for the same reason. The two
//! refusals answer `403` carrying [`ACCOUNT_DISABLED`] and
//! [`PASSWORD_CHANGE_REQUIRED`], which is what the browser routes on.
//!
//! Platform tokens are deliberately untouched by either state. A platform
//! application belongs to a team, not to a person, and its token acts as that
//! team through `roles.yml`; silencing a team's integrations because one of
//! its members was offboarded would be a surprise nobody asked for. Revoking
//! them is the team's own act, in the Developers section.
//!
//! The administrative surface is organization-scoped and lives in
//! [`crate::tenancy`]; see `docs/tenancy.md`.

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::model::User;
use crate::auth::session;
use crate::http::ApiError;
use crate::schema::users;

/// The error code a disabled account's refusal carries.
pub const ACCOUNT_DISABLED: &str = "account_disabled";

/// The error code the demand for a password change carries.
pub const PASSWORD_CHANGE_REQUIRED: &str = "password_change_required";

/// Refuses the request when the account is disabled.
///
/// The one check the password-change route runs, since a disabled account may
/// not act at all, while the account that owes a rotation is exactly the one
/// that route exists for.
///
/// # Errors
/// Returns a `403` carrying [`ACCOUNT_DISABLED`] for a disabled account.
pub(crate) fn require_enabled(user: &User) -> Result<(), ApiError> {
    if user.disabled_at.is_some() {
        return Err(disabled());
    }
    Ok(())
}

/// Refuses the request when the account is disabled or owes a rotation.
///
/// # Errors
/// Returns a `403` carrying [`ACCOUNT_DISABLED`] or
/// [`PASSWORD_CHANGE_REQUIRED`], so the browser can tell a dead end from a
/// detour.
pub(crate) fn require_usable(user: &User) -> Result<(), ApiError> {
    require_enabled(user)?;
    if user.password_change_required {
        return Err(
            ApiError::forbidden("Choose a new password before you continue.")
                .with_code(PASSWORD_CHANGE_REQUIRED),
        );
    }
    Ok(())
}

/// Refuses to hand a session to an account an administrator has disabled.
///
/// Reads the account rather than taking one, because the paths that mint a
/// session hold an id and nothing else: a verified second factor, a claimed
/// passkey, a consumed sign-in code, an OpenID Connect subject. One check
/// where they meet covers all of them, and covers the one added next.
///
/// # Errors
/// Returns a `403` carrying [`ACCOUNT_DISABLED`] for a disabled account, and a
/// `500` when the lookup fails.
pub(crate) async fn require_enabled_to_sign_in(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), ApiError> {
    let disabled_at: Option<chrono::DateTime<Utc>> = users::table
        .find(user_id)
        .select(users::disabled_at)
        .first(connection)
        .await
        .map_err(|error| {
            tracing::error!(
                error.message = %error,
                "account status lookup failed: {{error.message}}",
            );
            ApiError::internal()
        })?;

    if disabled_at.is_some() {
        return Err(disabled());
    }
    Ok(())
}

/// Disables the account and revokes every session it holds.
///
/// Run it inside a transaction: an account disabled while its sessions
/// survived is disabled in name only. Re-disabling an account is allowed and
/// re-stamps the moment, which is the honest reading of a column that records
/// when the account was last taken out of use.
///
/// # Errors
/// Returns the query's error, which the caller renders as a `500`.
pub(crate) async fn disable(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<User, diesel::result::Error> {
    let user: User = diesel::update(users::table.find(user_id))
        .set(users::disabled_at.eq(Utc::now()))
        .returning(User::as_returning())
        .get_result(connection)
        .await?;

    session::delete_all_for_user(connection, user_id).await?;

    Ok(user)
}

/// Returns the account to use, leaving any password demand in place.
///
/// Nothing is restored beyond the ability to sign in: the sessions disabling
/// revoked are gone for good, so the person signs in again.
///
/// # Errors
/// Returns the query's error, which the caller renders as a `500`.
pub(crate) async fn enable(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<User, diesel::result::Error> {
    diesel::update(users::table.find(user_id))
        .set(users::disabled_at.eq(None::<chrono::DateTime<Utc>>))
        .returning(User::as_returning())
        .get_result(connection)
        .await
}

/// Demands a password change before the account may do anything else.
///
/// The account keeps its sessions: it can sign in, and the one route it can
/// reach is the change that clears the demand.
///
/// # Errors
/// Returns the query's error, which the caller renders as a `500`.
pub(crate) async fn demand_password_change(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<User, diesel::result::Error> {
    diesel::update(users::table.find(user_id))
        .set(users::password_change_required.eq(true))
        .returning(User::as_returning())
        .get_result(connection)
        .await
}

/// The refusal a disabled account meets, wherever it is met.
///
/// Says what happened and who can undo it, and nothing about which
/// administrator did it or when: the account holder's remedy is the same
/// either way.
fn disabled() -> ApiError {
    ApiError::forbidden("This account has been disabled. Contact an administrator.")
        .with_code(ACCOUNT_DISABLED)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use chrono::Utc;
    use uuid::Uuid;

    use super::{ACCOUNT_DISABLED, PASSWORD_CHANGE_REQUIRED, require_enabled, require_usable};
    use crate::auth::model::User;

    fn user() -> User {
        User {
            id: Uuid::new_v4(),
            email: "sample@example.com".to_owned(),
            password_hash: "$argon2id$hash".to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            email_verified_at: None,
            first_name: None,
            last_name: None,
            time_zone: "UTC".to_owned(),
            locale: "en-US".to_owned(),
            disabled_at: None,
            password_change_required: false,
        }
    }

    #[test]
    fn a_usable_account_passes_both_checks() {
        let user = user();
        require_enabled(&user).expect("an enabled account must pass");
        require_usable(&user).expect("an account owing nothing must pass");
    }

    #[test]
    fn a_disabled_account_is_refused_everywhere() {
        let mut user = user();
        user.disabled_at = Some(Utc::now());

        for error in [
            require_enabled(&user).expect_err("a disabled account must be refused"),
            require_usable(&user).expect_err("a disabled account must be refused"),
        ] {
            assert_eq!(error.status(), StatusCode::FORBIDDEN);
            assert_eq!(error.code(), Some(ACCOUNT_DISABLED));
        }
    }

    #[test]
    fn a_flagged_account_is_refused_only_where_the_rotation_is_enforced() {
        let mut user = user();
        user.password_change_required = true;

        require_enabled(&user).expect("the password change itself must stay reachable");

        let error = require_usable(&user).expect_err("a flagged account must be refused");
        assert_eq!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(error.code(), Some(PASSWORD_CHANGE_REQUIRED));
    }

    #[test]
    fn disabling_outranks_the_rotation_demand() {
        let mut user = user();
        user.disabled_at = Some(Utc::now());
        user.password_change_required = true;

        let error = require_usable(&user).expect_err("a disabled account must be refused");
        assert_eq!(
            error.code(),
            Some(ACCOUNT_DISABLED),
            "sending a disabled account to change its password would be a dead end",
        );
    }
}
