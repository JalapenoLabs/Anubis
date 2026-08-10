//! Registration and login endpoints.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/auth`): `POST /register` creates an account, `POST /login` verifies
//! credentials. Login responds identically, in both message and timing,
//! whether the email is unknown or the password is wrong, so responses do not
//! leak which emails are registered. Session issuance arrives with the next
//! milestone step.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};

use crate::auth::model::{NewUser, User, UserResponse};
use crate::auth::password;
use crate::db::DbPool;
use crate::http::ApiError;
use crate::schema::users;

/// Upper bound from RFC 3696; anything longer cannot be a deliverable address.
const MAX_EMAIL_CHARS: usize = 320;

/// Returns the authentication routes for an application to mount.
pub fn router(pool: DbPool) -> Router {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .with_state(AuthState { pool })
}

#[derive(Clone)]
struct AuthState {
    pool: DbPool,
}

#[derive(Deserialize)]
struct CredentialsBody {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

async fn register(
    State(state): State<AuthState>,
    Json(body): Json<CredentialsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let credentials = validate_credentials(body)?;

    let password_hash = password::hash(credentials.password)
        .await
        .map_err(log_internal)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let created: User = diesel::insert_into(users::table)
        .values(NewUser {
            email: &credentials.email,
            password_hash: &password_hash,
        })
        .returning(User::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _details) => {
                ApiError::conflict("That email address is already registered.")
            }
            other => log_internal(other),
        })?;

    let body = UserBody {
        user: UserResponse::from(&created),
    };
    Ok((StatusCode::CREATED, Json(body)))
}

async fn login(
    State(state): State<AuthState>,
    Json(body): Json<CredentialsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let credentials = validate_credentials(body)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let user: Option<User> = users::table
        .filter(users::email.eq(&credentials.email))
        .select(User::as_select())
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;

    let Some(user) = user else {
        // Burn the same CPU as a real check so timing does not reveal
        // whether the email is registered.
        password::verify_against_dummy(credentials.password).await;
        return Err(invalid_credentials());
    };

    let matched = password::verify(credentials.password, user.password_hash.clone())
        .await
        .map_err(log_internal)?;

    if !matched {
        return Err(invalid_credentials());
    }

    let body = UserBody {
        user: UserResponse::from(&user),
    };
    Ok((StatusCode::OK, Json(body)))
}

struct ValidCredentials {
    email: String,
    password: String,
}

impl std::fmt::Debug for ValidCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidCredentials")
            .field("email", &self.email)
            .field("password", &"...")
            .finish()
    }
}

/// Normalizes and validates a credentials payload.
///
/// Email normalization (trim + lowercase) happens here so registration and
/// login always agree on the stored form.
fn validate_credentials(body: CredentialsBody) -> Result<ValidCredentials, ApiError> {
    let email = body.email.trim().to_lowercase();

    if email.is_empty() || email.chars().count() > MAX_EMAIL_CHARS {
        return Err(ApiError::validation("Enter a valid email address."));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(ApiError::validation("Enter a valid email address."));
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.contains(char::is_whitespace)
    {
        return Err(ApiError::validation("Enter a valid email address."));
    }

    let password_chars = body.password.chars().count();
    if password_chars < password::MIN_PASSWORD_CHARS {
        return Err(ApiError::validation(format!(
            "Passwords must be at least {} characters.",
            password::MIN_PASSWORD_CHARS
        )));
    }
    if password_chars > password::MAX_PASSWORD_CHARS {
        return Err(ApiError::validation(format!(
            "Passwords must be at most {} characters.",
            password::MAX_PASSWORD_CHARS
        )));
    }

    Ok(ValidCredentials {
        email,
        password: body.password,
    })
}

fn invalid_credentials() -> ApiError {
    ApiError::unauthorized("Invalid email or password.")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "auth request failed: {{error.message}}",
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{CredentialsBody, validate_credentials};

    fn body(email: &str, password: &str) -> CredentialsBody {
        CredentialsBody {
            email: email.to_owned(),
            password: password.to_owned(),
        }
    }

    #[test]
    fn emails_are_normalized_to_trimmed_lowercase() {
        let valid = validate_credentials(body("  Alex@Example.COM ", "long enough password"))
            .expect("valid credentials must pass");
        assert_eq!(valid.email, "alex@example.com");
        assert_eq!(valid.password, "long enough password");
    }

    #[test]
    fn structurally_broken_emails_are_rejected() {
        for email in [
            "",
            "no-at-sign",
            "@no-local",
            "no-domain@",
            "two@@ats",
            "sp ace@example.com",
        ] {
            let error = validate_credentials(body(email, "long enough password"))
                .expect_err("broken emails must fail");
            assert_eq!(
                error.status(),
                StatusCode::BAD_REQUEST,
                "for input {email:?}"
            );
        }
    }

    #[test]
    fn short_and_absurdly_long_passwords_are_rejected() {
        let error = validate_credentials(body("alex@example.com", "short"))
            .expect_err("short passwords must fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);

        let long_password = "x".repeat(513);
        let error = validate_credentials(body("alex@example.com", &long_password))
            .expect_err("oversized passwords must fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }
}
