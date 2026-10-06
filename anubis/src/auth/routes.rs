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
//!
//! Every route above that an attacker can drive without credentials carries a
//! per-client budget from [`crate::rate_limit`], declared beside the route.
//! The endpoints that send email additionally charge the address they would
//! mail, because rotating client addresses is how one inbox gets bombed.

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

use crate::audit;
use crate::auth::extract::CurrentUser;
use crate::auth::model::{NewUser, User, UserResponse};
use crate::auth::secret_box::SecretKey;
use crate::auth::session::SignInMethod;
use crate::auth::user_token::TokenPurpose;
use crate::auth::{password, policy, session, user_token};
use crate::config::{AppConfig, Environment};
use crate::db::DbPool;
use crate::http::ApiError;
use crate::mail::{Email, EmailKind, Mailer};
use crate::rate_limit::{Budget, RateLimiter};
use crate::schema::users;

/// Returns the authentication routes for an application to mount.
///
/// The mailer delivers verification and reset email; the config supplies the
/// environment (whether session cookies are `Secure`) and the public base URL
/// embedded in email links.
///
/// The limiter is passed in rather than built here because a budget is only a
/// budget when one balance covers every surface that spends it: the inbox
/// budget an application charges from `/auth` is the same one
/// [`crate::tenancy::router`] charges when it mails an invitation. Build one
/// with `RateLimiter::new(&config.rate_limit)` and hand it to both.
pub fn router(
    pool: DbPool,
    mailer: Mailer,
    config: &AppConfig,
    rate_limit: &RateLimiter,
) -> Router {
    let state = AuthState {
        pool: pool.clone(),
        environment: config.environment,
        mailer,
        app_url: config.app_url.clone(),
        secret_key: config.secret_key.clone(),
        oauth: crate::auth::oauth::Runtime::new(config.oauth.clone()),
        rate_limit: rate_limit.clone(),
        hasher: password::Hasher::new(config.password_hash_concurrency),
    };

    Router::new()
        .route(
            "/register",
            post(register).layer(rate_limit.layer(Budget::Registration)),
        )
        .route(
            "/login",
            post(login).layer(rate_limit.layer(Budget::Credentials)),
        )
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route(
            "/verify-email/request",
            post(request_email_verification).layer(rate_limit.layer(Budget::EmailPerClient)),
        )
        .route("/verify-email/confirm", post(confirm_email_verification))
        .route(
            "/password-reset/request",
            post(request_password_reset).layer(rate_limit.layer(Budget::EmailPerClient)),
        )
        .route("/password-reset/confirm", post(confirm_password_reset))
        .merge(crate::auth::account::router())
        .merge(crate::auth::email_code::router(rate_limit))
        .merge(crate::auth::mfa::router(rate_limit))
        .merge(crate::auth::oauth::router())
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
    /// Seals the secrets auth must read back, today the TOTP seeds.
    pub(crate) secret_key: SecretKey,
    /// The configured OpenID Connect providers and their discovery cache.
    pub(crate) oauth: crate::auth::oauth::Runtime,
    /// Budgets the handlers charge themselves, keyed by the target address.
    pub(crate) rate_limit: RateLimiter,
    /// The gate every argon2 computation passes through; see
    /// [`crate::auth::password`].
    pub(crate) hasher: password::Hasher,
}

#[derive(Deserialize)]
struct CredentialsBody {
    email: String,
    password: String,
}

/// What a sign-up carries: the credentials, and the preferences the browser
/// already knows.
///
/// The time zone and locale are optional so a client that sends neither still
/// registers, and they are never a reason to refuse one: see
/// [`registration_preference`].
#[derive(Deserialize)]
struct RegisterBody {
    email: String,
    password: String,
    time_zone: Option<String>,
    locale: Option<String>,
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
    context: audit::Context,
    Json(body): Json<RegisterBody>,
) -> Result<impl IntoResponse, ApiError> {
    let time_zone = registration_preference(body.time_zone, "time zone");
    let locale = registration_preference(body.locale, "locale");
    let credentials = validate_credentials(CredentialsBody {
        email: body.email,
        password: body.password,
    })?;

    let password_hash = state.hasher.hash(credentials.password).await?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    // One transaction creates the user and their personal organization, so a
    // half-bootstrapped account can never exist.
    let created: User = connection
        .transaction(async |transaction| {
            let user: User = diesel::insert_into(users::table)
                .values(NewUser {
                    email: &credentials.email,
                    password_hash: &password_hash,
                    time_zone: time_zone.as_deref(),
                    locale: locale.as_deref(),
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

    let jar = signed_in_jar(
        &state,
        &mut connection,
        &context,
        &created,
        SignInMethod::Registration,
    )
    .await?;
    let body = UserBody {
        user: UserResponse::load(&mut connection, &created)
            .await
            .map_err(log_internal)?,
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
    context: audit::Context,
    Json(body): Json<CredentialsBody>,
) -> Result<axum::response::Response, ApiError> {
    // Signing in judges nothing about the shape of what was typed. The length
    // rule is a sign-up rule, and quoting it here tells somebody guessing which
    // guesses are worth making; a malformed address answering differently from
    // an unknown one is a second thing learned. Every refusal is the same 401.
    //
    // The one bound kept is the ceiling, so a megabyte of "password" is refused
    // before it reaches argon2. It answers the same way, and since it is
    // decided before any lookup it says nothing about whether the account exists.
    if body.password.chars().count() > password::MAX_PASSWORD_CHARS {
        tracing::debug!("a sign-in was refused for a password over the ceiling");
        return Err(invalid_credentials());
    }
    let Some(email) = policy::normalize_email(&body.email) else {
        // The same CPU as a real check, as for an address nobody holds below.
        state.hasher.verify_against_dummy(body.password).await?;
        return Err(invalid_credentials());
    };
    let credentials = ValidCredentials {
        email,
        password: body.password,
    };

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
        // whether the email is registered. A shed check propagates, so an
        // overloaded server answers both paths the same way.
        state
            .hasher
            .verify_against_dummy(credentials.password)
            .await?;
        return Err(invalid_credentials());
    };

    let matched = state
        .hasher
        .verify(credentials.password, user.password_hash.clone())
        .await?;

    if !matched {
        return Err(invalid_credentials());
    }

    // A confirmed second factor turns the session into a challenge.
    if crate::auth::mfa::confirmed_secret(&mut connection, &state.secret_key, user.id)
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

    let jar = signed_in_jar(
        &state,
        &mut connection,
        &context,
        &user,
        SignInMethod::Password,
    )
    .await?;
    let body = UserBody {
        user: UserResponse::load(&mut connection, &user)
            .await
            .map_err(log_internal)?,
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

async fn me(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<UserBody>, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    Ok(Json(UserBody {
        user: UserResponse::load(&mut connection, &user)
            .await
            .map_err(log_internal)?,
    }))
}

/// How long an account waits between verification emails it asked for.
///
/// Per account and kept in the database, because the per-client budget on
/// the route lives in one process and resets on every deploy, and what is
/// being rationed is one inbox. Measured from the latest link, so the one
/// sent at registration counts: a person who signs up and immediately asks
/// again is told to check the email that is already on its way.
const VERIFICATION_RESEND_COOLDOWN: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// What a resend decided while it held the account.
enum Resend {
    /// A fresh token, to be mailed once the transaction commits.
    Issued(String),
    /// The previous link is too recent; this long remains.
    CoolingDown(chrono::TimeDelta),
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
    let resend = connection
        .transaction(async |transaction| {
            // Hold the account so two resends queue: without it both read the
            // same last-sent time, both pass, and the inbox gets two links.
            let _held: Uuid = users::table
                .find(user.id)
                .select(users::id)
                .for_update()
                .first(transaction)
                .await?;

            let last_sent =
                user_token::issued_at(transaction, user.id, TokenPurpose::EmailVerification)
                    .await?;
            if let Some(sent_at) = last_sent {
                let waited = Utc::now() - sent_at;
                if waited < VERIFICATION_RESEND_COOLDOWN {
                    return Ok(Resend::CoolingDown(VERIFICATION_RESEND_COOLDOWN - waited));
                }
            }

            let token =
                user_token::issue(transaction, user.id, TokenPurpose::EmailVerification).await?;
            Ok::<Resend, diesel::result::Error>(Resend::Issued(token))
        })
        .await
        .map_err(log_internal)?;

    let token = match resend {
        Resend::Issued(token) => token,
        Resend::CoolingDown(remaining) => {
            tracing::debug!(
                user.id = %user.id,
                "verification resend refused inside the cooldown for {{user.id}}",
            );
            return Err(ApiError::too_many_requests(
                remaining.to_std().unwrap_or_default(),
            ));
        }
    };
    deliver_verification_link(&state, &user, &token).await;

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
        user: UserResponse::load(&mut connection, &user)
            .await
            .map_err(log_internal)?,
    }))
}

async fn request_password_reset(
    State(state): State<AuthState>,
    Json(body): Json<EmailOnlyBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = body.email.trim().to_lowercase();

    // Charged before the lookup, so the answer cannot depend on whether the
    // address belongs to an account, and so an attacker rotating client
    // addresses still meets one budget per inbox.
    state.rate_limit.check_recipient(&email)?;

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
            Email::new(
                EmailKind::ResetPassword,
                user.email.clone(),
                "Reset your password",
                format!(
                    "Someone requested a password reset for this account.\n\n\
                     Set a new password within 30 minutes: {link}\n\n\
                     If this wasn't you, ignore this email; your password is unchanged.",
                ),
            )
            .with_param("link", &link),
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

    let password_hash = state.hasher.hash(body.password).await?;

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

    deliver_verification_link(state, user, &token).await;
}

/// Emails the verification link for a token that was already issued.
///
/// Split from [`send_verification_email`] so a resend can issue its token
/// inside the transaction that decides the cooldown and mail it after that
/// transaction commits, rather than holding a row lock across a network call.
async fn deliver_verification_link(state: &AuthState, user: &User, token: &str) {
    let link = format!("{}/verify-email?token={token}", state.app_url);
    deliver(
        &state.mailer,
        Email::new(
            EmailKind::VerifyEmailAddress,
            user.email.clone(),
            "Verify your email address",
            format!(
                "Welcome! Confirm this email address within 3 days: {link}\n\n\
                 If you didn't create this account, ignore this email.",
            ),
        )
        .with_param("link", &link),
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

/// Creates a session for `user`, records the sign-in, and returns a jar
/// carrying the session's cookie.
///
/// Every path that signs somebody in comes through here, which is what makes
/// the [`audit::SESSION_CREATED`] record complete rather than a convention
/// each path has to remember. The session and its record commit together, so
/// a browser never holds a session the log does not know about. `context` is
/// the request's, so the record carries the address, browser and location the
/// sign-in came from; see [`crate::server::origin`].
pub(crate) async fn signed_in_jar(
    state: &AuthState,
    connection: &mut diesel_async::AsyncPgConnection,
    context: &audit::Context,
    user: &User,
    method: SignInMethod,
) -> Result<CookieJar, ApiError> {
    let actor = context.by(user);
    let label = audit::person_label(
        user.first_name.as_deref(),
        user.last_name.as_deref(),
        &user.email,
    );

    let token = connection
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            let token = session::create(transaction, user.id).await?;
            audit::record(
                transaction,
                &actor,
                &audit::Event::new(audit::SESSION_CREATED, "User")
                    .subject(user.id)
                    .label(&label)
                    .changes(audit::Changes::new().field(
                        "method",
                        serde_json::Value::Null,
                        method.as_str(),
                    )),
            )
            .await?;
            Ok(token)
        })
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

/// Keeps a preference sent at sign-up, or drops it so the column default holds.
///
/// Held to the bound `PATCH /profile` applies, but a value outside it is
/// dropped rather than refused: the browser chose it, not the person, and a
/// sign-up that failed over a time zone would lose the account for a setting
/// they can change in a second afterwards.
fn registration_preference(raw: Option<String>, label: &str) -> Option<String> {
    let trimmed = raw?.trim().to_owned();
    if trimmed.is_empty() || trimmed.chars().count() > crate::auth::account::MAX_FIELD_CHARS {
        tracing::debug!(
            preference.label = label,
            "registration ignored an unusable {{preference.label}} and kept the default",
        );
        return None;
    }
    Some(trimmed)
}

/// Normalizes (trim + lowercase) and structurally validates an email address.
pub(crate) fn validate_email(raw: &str) -> Result<String, ApiError> {
    policy::normalize_email(raw).ok_or_else(|| ApiError::validation("Enter a valid email address."))
}

/// Enforces the password length policy shared by registration and reset.
pub(crate) fn validate_password(candidate: &str) -> Result<(), ApiError> {
    policy::check_password_policy(candidate)
        .map_err(|violation| ApiError::validation(format!("Passwords must be {violation}.")))
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
