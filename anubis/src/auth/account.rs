//! Signed-in account management: profile, credentials, sessions, deletion.
//!
//! These routes merge into the auth router (conventionally under `/auth`):
//!
//! | Route | Effect |
//! |---|---|
//! | `PATCH /profile` | Update name, time zone, and locale |
//! | `POST /change-password` | Rotate the password; other sessions sign out |
//! | `POST /change-email/request` | Email a confirmation link to the new address |
//! | `POST /change-email/confirm` | Swap to the confirmed address |
//! | `GET /sessions` | List the user's sessions, marking the current one |
//! | `DELETE /sessions/{session_id}` | Revoke one session |
//! | `DELETE /account` | Delete the account (password-confirmed) |
//!
//! `POST /change-password` is the one route an account owing a password
//! change may still reach, and the change clears that demand. See
//! [`crate::auth::account_status`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::extract::RotatingUser;
use crate::auth::model::{User, UserResponse};
use crate::auth::routes::{AuthState, validate_email, validate_password};
use crate::auth::user_token::TokenPurpose;
use crate::auth::{CurrentUser, password, session, token, user_token};
use crate::http::ApiError;
use crate::mail::Email;
use crate::schema::{sessions, users};

/// Longest accepted name, time zone, or locale value.
const MAX_FIELD_CHARS: usize = 100;

pub(crate) fn router() -> Router<AuthState> {
    Router::new()
        .merge(crate::auth::avatar::account_routes())
        .route("/profile", patch(update_profile))
        .route("/change-password", post(change_password))
        .route("/change-email/request", post(request_email_change))
        .route("/change-email/confirm", post(confirm_email_change))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{session_id}", delete(revoke_session))
        .route("/account", delete(delete_account))
}

#[derive(Deserialize)]
struct ProfileBody {
    /// Absent fields stay unchanged; an empty string clears a name.
    first_name: Option<String>,
    last_name: Option<String>,
    time_zone: Option<String>,
    locale: Option<String>,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

#[derive(Serialize)]
struct MessageBody {
    message: &'static str,
}

async fn update_profile(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ProfileBody>,
) -> Result<impl IntoResponse, ApiError> {
    let first_name = normalize_name(body.first_name, user.first_name)?;
    let last_name = normalize_name(body.last_name, user.last_name)?;
    let time_zone = normalize_required(body.time_zone, user.time_zone, "time zone")?;
    let locale = normalize_required(body.locale, user.locale, "locale")?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let updated: User = diesel::update(users::table.find(user.id))
        .set((
            users::first_name.eq(first_name),
            users::last_name.eq(last_name),
            users::time_zone.eq(time_zone),
            users::locale.eq(locale),
        ))
        .returning(User::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(UserBody {
        user: UserResponse::from(&updated),
    }))
}

/// Applies a name edit: absent keeps the current value, blank clears it.
fn normalize_name(
    incoming: Option<String>,
    current: Option<String>,
) -> Result<Option<String>, ApiError> {
    let Some(raw) = incoming else {
        return Ok(current);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_FIELD_CHARS {
        return Err(ApiError::validation(
            "Names must be 100 characters or fewer.",
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Applies an edit to a required field: absent keeps the current value.
fn normalize_required(
    incoming: Option<String>,
    current: String,
    label: &str,
) -> Result<String, ApiError> {
    let Some(raw) = incoming else {
        return Ok(current);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_FIELD_CHARS {
        return Err(ApiError::validation(format!(
            "Provide a valid {label} (1 to 100 characters)."
        )));
    }
    Ok(trimmed.to_owned())
}

#[derive(Deserialize)]
struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}

/// Rotates the password, and clears any demand that it be rotated.
///
/// The one route [`RotatingUser`] exempts from that demand, because refusing
/// it would leave a flagged account with nothing it could do. Clearing the
/// flag is a side effect of the change rather than a call of its own: the
/// demand is satisfied by the act, and a second endpoint could only disagree
/// with it.
async fn change_password(
    State(state): State<AuthState>,
    RotatingUser(user): RotatingUser,
    jar: CookieJar,
    Json(body): Json<ChangePasswordBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate_password(&body.new_password)?;
    verify_password_or_reject(&state.hasher, &user, body.current_password).await?;

    let password_hash = state.hasher.hash(body.new_password).await?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::update(users::table.find(user.id))
        .set((
            users::password_hash.eq(&password_hash),
            users::password_change_required.eq(false),
        ))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    // Other browsers sign out; the one that made the change stays in.
    if let Some(cookie) = jar.get(session::SESSION_COOKIE) {
        session::delete_all_except(&mut connection, user.id, cookie.value())
            .await
            .map_err(log_internal)?;
    }

    Ok(Json(MessageBody {
        message: "Your password has been changed. Other sessions were signed out.",
    }))
}

#[derive(Deserialize)]
struct EmailChangeRequestBody {
    new_email: String,
    password: String,
}

async fn request_email_change(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<EmailChangeRequestBody>,
) -> Result<impl IntoResponse, ApiError> {
    let new_email = validate_email(&body.new_email)?;
    verify_password_or_reject(&state.hasher, &user, body.password).await?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let taken: bool = diesel::select(diesel::dsl::exists(
        users::table.filter(users::email.eq(&new_email)),
    ))
    .get_result(&mut connection)
    .await
    .map_err(log_internal)?;
    if taken {
        return Err(ApiError::conflict(
            "That email address is already registered.",
        ));
    }

    let raw_token = user_token::issue_with_payload(
        &mut connection,
        user.id,
        TokenPurpose::EmailChange,
        Some(&new_email),
    )
    .await
    .map_err(log_internal)?;

    let link = format!("{}/change-email?token={raw_token}", state.app_url);
    let mail = Email {
        to: new_email.clone(),
        subject: "Confirm your new email address".to_owned(),
        text_body: format!(
            "Confirm this address to make it the sign-in email for your account.\n\n\
             Confirm within 1 hour: {link}\n\n\
             If you weren't expecting this, ignore this email.",
        ),
    };
    if let Err(error) = state.mailer.send(mail).await {
        tracing::error!(
            error.message = %error,
            "failed to deliver the email-change confirmation: {{error.message}}",
        );
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(MessageBody {
            message: "Check the new address for a confirmation link.",
        }),
    ))
}

#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

async fn confirm_email_change(
    State(state): State<AuthState>,
    Json(body): Json<TokenBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (user_id, payload) =
        user_token::consume(&mut connection, &body.token, TokenPurpose::EmailChange)
            .await
            .map_err(log_internal)?
            .ok_or_else(expired_link)?;
    let Some(new_email) = payload else {
        tracing::error!("an email-change token had no payload");
        return Err(ApiError::internal());
    };

    // Confirming the link proves control of the new address.
    let updated: User = diesel::update(users::table.find(user_id))
        .set((
            users::email.eq(&new_email),
            users::email_verified_at.eq(Utc::now()),
        ))
        .returning(User::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _details,
            ) => ApiError::conflict("That email address is already registered."),
            other => log_internal(other),
        })?;

    Ok(Json(UserBody {
        user: UserResponse::from(&updated),
    }))
}

#[derive(Serialize)]
struct SessionEntry {
    id: Uuid,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// True for the session making this request.
    current: bool,
}

#[derive(Serialize)]
struct SessionsBody {
    sessions: Vec<SessionEntry>,
}

async fn list_sessions(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let current_hash = jar
        .get(session::SESSION_COOKIE)
        .map(|cookie| token::hash(cookie.value()));

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let rows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>, String)> = sessions::table
        .filter(sessions::user_id.eq(user.id))
        .filter(sessions::expires_at.gt(Utc::now()))
        .select((
            sessions::id,
            sessions::created_at,
            sessions::expires_at,
            sessions::token_hash,
        ))
        .order(sessions::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let entries = rows
        .into_iter()
        .map(|(id, created_at, expires_at, token_hash)| SessionEntry {
            id,
            created_at,
            expires_at,
            current: current_hash.as_deref() == Some(token_hash.as_str()),
        })
        .collect();

    Ok(Json(SessionsBody { sessions: entries }))
}

async fn revoke_session(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let deleted = diesel::delete(
        sessions::table
            .filter(sessions::id.eq(session_id))
            .filter(sessions::user_id.eq(user.id)),
    )
    .execute(&mut connection)
    .await
    .map_err(log_internal)?;

    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DeleteAccountBody {
    password: String,
}

/// Deletes the account and settles the tenancy it leaves behind.
///
/// One transaction removes the user's memberships, dissolves organizations
/// nobody can reach any more, keeps the surviving ones administrable, and then
/// deletes the user, so the account can never be half gone. The rules live in
/// `docs/tenancy.md`.
async fn delete_account(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    jar: CookieJar,
    Json(body): Json<DeleteAccountBody>,
) -> Result<impl IntoResponse, ApiError> {
    verify_password_or_reject(&state.hasher, &user, body.password).await?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    connection
        .transaction(async |transaction| {
            crate::tenancy::settle_departure(transaction, user.id).await?;
            diesel::delete(users::table.find(user.id))
                .execute(transaction)
                .await
        })
        .await
        .map_err(log_internal)?;

    let removal = Cookie::build((session::SESSION_COOKIE, ""))
        .path("/")
        .build();
    Ok((jar.remove(removal), StatusCode::NO_CONTENT))
}

/// Verifies the user's password, rejecting with a uniform 401 on mismatch.
async fn verify_password_or_reject(
    hasher: &password::Hasher,
    user: &User,
    candidate: String,
) -> Result<(), ApiError> {
    let matched = hasher.verify(candidate, user.password_hash.clone()).await?;
    if matched {
        Ok(())
    } else {
        Err(ApiError::unauthorized("Your password was incorrect."))
    }
}

fn expired_link() -> ApiError {
    ApiError::validation("That link is invalid or has expired. Request a new one.")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "account request failed: {{error.message}}",
    );
    ApiError::internal()
}
