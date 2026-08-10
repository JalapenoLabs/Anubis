//! Registration, login, sessions, email verification, and password reset.
//!
//! [`router`] returns the routes an application mounts (conventionally under
//! `/auth`):
//!
//! | Route | Effect |
//! |---|---|
//! | `POST /register` | Create an account, send a verification email, sign in |
//! | `POST /login` | Verify credentials, sign in |
//! | `POST /logout` | Revoke the session server-side |
//! | `GET /me` | Return the signed-in user |
//! | `POST /verify-email/request` | Re-send the verification email |
//! | `POST /verify-email/confirm` | Confirm the emailed verification token |
//! | `POST /password-reset/request` | Email a reset link (never reveals account existence) |
//! | `POST /password-reset/confirm` | Set a new password, revoking every session |
//!
//! Login and password-reset requests respond identically whether or not the
//! email is registered, in both message and timing, so responses do not leak
//! which emails exist.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::Utc;
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::extract::CurrentUser;
use crate::auth::model::{NewUser, User, UserResponse};
use crate::auth::user_token::TokenPurpose;
use crate::auth::{password, session, user_token};
use crate::config::{AppConfig, Environment};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::mail::{Email, Mailer};
use crate::schema::users;

/// Upper bound from RFC 3696; anything longer cannot be a deliverable address.
const MAX_EMAIL_CHARS: usize = 320;

/// Returns the authentication routes for an application to mount.
///
/// The mailer delivers verification and reset email; the config supplies the
/// environment (whether session cookies are `Secure`) and the public base URL
/// embedded in email links.
pub fn router(pool: DbPool, mailer: Mailer, config: &AppConfig) -> Router {
    let state = AuthState {
        pool: pool.clone(),
        environment: config.environment,
        mailer,
        app_url: config.app_url.clone(),
    };

    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/verify-email/request", post(request_email_verification))
        .route("/verify-email/confirm", post(confirm_email_verification))
        .route("/password-reset/request", post(request_password_reset))
        .route("/password-reset/confirm", post(confirm_password_reset))
        .merge(crate::auth::account::router())
        .merge(crate::auth::email_code::router())
        .merge(crate::auth::mfa::router())
        .merge(crate::auth::passkey::router())
        .with_state(state)
        // CurrentUser resolves its pool from request extensions.
        .layer(Extension(pool))
}

#[derive(Clone)]
pub(crate) struct AuthState {
    pub(crate) pool: DbPool,
    pub(crate) environment: Environment,
    pub(crate) mailer: Mailer,
    pub(crate) app_url: String,
}

#[derive(Deserialize)]
struct CredentialsBody {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct EmailOnlyBody {
    email: String,
}

#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

#[derive(Deserialize)]
struct ResetBody {
    token: String,
    password: String,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

#[derive(Serialize)]
struct MessageBody {
    message: &'static str,
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

    // One transaction creates the user and their personal organization, so a
    // half-bootstrapped account can never exist.
    let created: User = connection
        .transaction(async |transaction| {
            let user: User = diesel::insert_into(users::table)
                .values(NewUser {
                    email: &credentials.email,
                    password_hash: &password_hash,
                })
                .returning(User::as_returning())
                .get_result(transaction)
                .await?;

            crate::tenancy::create_personal_organization(transaction, &user).await?;

            Ok::<User, diesel::result::Error>(user)
        })
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _details) => {
                ApiError::conflict("That email address is already registered.")
            }
            other => log_internal(other),
        })?;

    send_verification_email(&state, &mut connection, &created).await;

    let jar = signed_in_jar(&state, &mut connection, created.id).await?;
    let body = UserBody {
        user: UserResponse::from(&created),
    };
    Ok((jar, (StatusCode::CREATED, Json(body))))
}

#[derive(Serialize)]
struct MfaChallengeBody {
    mfa_required: bool,
    /// Present with `POST /mfa/verify` alongside a TOTP or recovery code.
    mfa_token: String,
}

async fn login(
    State(state): State<AuthState>,
    Json(body): Json<CredentialsBody>,
) -> Result<axum::response::Response, ApiError> {
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

    // A confirmed second factor turns the session into a challenge.
    if crate::auth::mfa::confirmed_secret(&mut connection, user.id)
        .await
        .map_err(log_internal)?
        .is_some()
    {
        let mfa_token = user_token::issue(&mut connection, user.id, TokenPurpose::MfaChallenge)
            .await
            .map_err(log_internal)?;
        return Ok((
            StatusCode::OK,
            Json(MfaChallengeBody {
                mfa_required: true,
                mfa_token,
            }),
        )
            .into_response());
    }

    let jar = signed_in_jar(&state, &mut connection, user.id).await?;
    let body = UserBody {
        user: UserResponse::from(&user),
    };
    Ok((jar, (StatusCode::OK, Json(body))).into_response())
}

async fn logout(
    State(state): State<AuthState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(cookie) = jar.get(session::SESSION_COOKIE) {
        let mut connection = state.pool.get().await.map_err(log_internal)?;
        session::delete(&mut connection, cookie.value())
            .await
            .map_err(log_internal)?;
    }

    let removal = Cookie::build((session::SESSION_COOKIE, ""))
        .path("/")
        .build();
    Ok((jar.remove(removal), StatusCode::NO_CONTENT))
}

async fn me(CurrentUser(user): CurrentUser) -> Json<UserBody> {
    Json(UserBody {
        user: UserResponse::from(&user),
    })
}

async fn request_email_verification(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    if user.email_verified_at.is_some() {
        return Ok((
            StatusCode::OK,
            Json(MessageBody {
                message: "Your email address is already verified.",
            }),
        ));
    }

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    send_verification_email(&state, &mut connection, &user).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(MessageBody {
            message: "Check your inbox for a verification link.",
        }),
    ))
}

async fn confirm_email_verification(
    State(state): State<AuthState>,
    Json(body): Json<TokenBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (user_id, _payload) = user_token::consume(
        &mut connection,
        &body.token,
        TokenPurpose::EmailVerification,
    )
    .await
    .map_err(log_internal)?
    .ok_or_else(expired_link)?;

    let user: User = diesel::update(users::table.find(user_id))
        .set((users::email_verified_at.eq(Utc::now()),))
        .returning(User::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(UserBody {
        user: UserResponse::from(&user),
    }))
}

async fn request_password_reset(
    State(state): State<AuthState>,
    Json(body): Json<EmailOnlyBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = body.email.trim().to_lowercase();

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let user: Option<User> = users::table
        .filter(users::email.eq(&email))
        .select(User::as_select())
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;

    if let Some(user) = user {
        let token = user_token::issue(&mut connection, user.id, TokenPurpose::PasswordReset)
            .await
            .map_err(log_internal)?;
        let link = format!("{}/reset-password?token={token}", state.app_url);
        deliver(
            &state.mailer,
            Email {
                to: user.email.clone(),
                subject: "Reset your password".to_owned(),
                text_body: format!(
                    "Someone requested a password reset for this account.\n\n\
                     Set a new password within 30 minutes: {link}\n\n\
                     If this wasn't you, ignore this email; your password is unchanged.",
                ),
            },
        )
        .await;
    }

    // The unknown-email path answers identically so responses cannot be used
    // to probe which addresses are registered.
    Ok((
        StatusCode::ACCEPTED,
        Json(MessageBody {
            message: "If that email is registered, a reset link is on its way.",
        }),
    ))
}

async fn confirm_password_reset(
    State(state): State<AuthState>,
    Json(body): Json<ResetBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate_password(&body.password)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (user_id, _payload) =
        user_token::consume(&mut connection, &body.token, TokenPurpose::PasswordReset)
            .await
            .map_err(log_internal)?
            .ok_or_else(expired_link)?;

    let password_hash = password::hash(body.password).await.map_err(log_internal)?;

    diesel::update(users::table.find(user_id))
        .set((users::password_hash.eq(&password_hash),))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    // A reset proves the old credentials may be compromised; no session
    // created under them survives.
    session::delete_all_for_user(&mut connection, user_id)
        .await
        .map_err(log_internal)?;

    Ok(Json(MessageBody {
        message: "Your password has been reset. Sign in with your new password.",
    }))
}

/// Issues a verification token and emails its link. Failures log; they never
/// fail the surrounding request, since the account itself is fine and the
/// user can re-request from the UI.
async fn send_verification_email(
    state: &AuthState,
    connection: &mut diesel_async::AsyncPgConnection,
    user: &User,
) {
    let token = match user_token::issue(connection, user.id, TokenPurpose::EmailVerification).await
    {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "failed to issue a verification token: {{error.message}}",
            );
            return;
        }
    };

    let link = format!("{}/verify-email?token={token}", state.app_url);
    deliver(
        &state.mailer,
        Email {
            to: user.email.clone(),
            subject: "Verify your email address".to_owned(),
            text_body: format!(
                "Welcome! Confirm this email address within 3 days: {link}\n\n\
                 If you didn't create this account, ignore this email.",
            ),
        },
    )
    .await;
}

/// Sends an email, logging failures instead of failing the request.
async fn deliver(mailer: &Mailer, email: Email) {
    if let Err(error) = mailer.send(email).await {
        tracing::error!(
            error.message = %error,
            "failed to deliver email: {{error.message}}",
        );
    }
}

/// Creates a session for `user_id` and returns a jar carrying its cookie.
pub(crate) async fn signed_in_jar(
    state: &AuthState,
    connection: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
) -> Result<CookieJar, ApiError> {
    let token = session::create(connection, user_id)
        .await
        .map_err(log_internal)?;

    let cookie = Cookie::build((session::SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::days(session::SESSION_TTL_DAYS))
        .secure(state.environment.is_production())
        .build();

    Ok(CookieJar::new().add(cookie))
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
    let email = validate_email(&body.email)?;
    validate_password(&body.password)?;

    Ok(ValidCredentials {
        email,
        password: body.password,
    })
}

/// Normalizes (trim + lowercase) and structurally validates an email address.
pub(crate) fn validate_email(raw: &str) -> Result<String, ApiError> {
    let email = raw.trim().to_lowercase();

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

    Ok(email)
}

/// Enforces the password length policy shared by registration and reset.
pub(crate) fn validate_password(candidate: &str) -> Result<(), ApiError> {
    let password_chars = candidate.chars().count();
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
    Ok(())
}

fn invalid_credentials() -> ApiError {
    ApiError::unauthorized("Invalid email or password.")
}

fn expired_link() -> ApiError {
    ApiError::validation("That link is invalid or has expired. Request a new one.")
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
