//! TOTP two-factor authentication: enrollment, challenge, recovery codes.
//!
//! Enrollment is a two-step handshake: `POST /mfa/totp/setup` issues a
//! secret (with an `otpauth://` URI and a QR at `GET /mfa/totp/qr.svg`), and
//! `POST /mfa/totp/confirm` activates it once the user proves their app
//! generates matching codes, returning single-use recovery codes. Only
//! confirmed enrollments gate login.
//!
//! When a confirmed enrollment exists, password login answers with a
//! short-lived challenge token instead of a session; `POST /mfa/verify`
//! exchanges the challenge plus a TOTP or recovery code for the session.
//! Challenges allow a few wrong codes before dying, so a typo never forces a
//! fresh password login.

use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use data_encoding::BASE32_NOPAD;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::model::UserResponse;
use crate::auth::routes::AuthState;
use crate::auth::user_token::TokenPurpose;
use crate::auth::{CurrentUser, password, token, totp, user_token};
use crate::http::ApiError;
use crate::schema::{user_mfa, user_recovery_codes, users};

/// Recovery codes issued at confirmation.
const RECOVERY_CODE_COUNT: usize = 10;

pub(crate) fn router() -> Router<AuthState> {
    Router::new()
        .route("/mfa", get(status))
        .route("/mfa/totp/setup", post(setup))
        .route("/mfa/totp/qr.svg", get(qr_svg))
        .route("/mfa/totp/confirm", post(confirm))
        .route("/mfa/totp/disable", post(disable))
        .route("/mfa/verify", post(verify_challenge))
}

#[derive(Serialize)]
struct StatusBody {
    totp_enabled: bool,
}

async fn status(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let enabled = confirmed_secret(&mut connection, user.id)
        .await
        .map_err(log_internal)?
        .is_some();

    Ok(Json(StatusBody {
        totp_enabled: enabled,
    }))
}

#[derive(Serialize)]
struct SetupBody {
    /// Base32 secret for manual entry into an authenticator app.
    secret: String,
    /// The provisioning URI behind the QR code.
    otpauth_uri: String,
}

async fn setup(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    if confirmed_secret(&mut connection, user.id)
        .await
        .map_err(log_internal)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "Two-factor auth is already enabled. Disable it before re-enrolling.",
        ));
    }

    let secret = totp::generate_secret();
    diesel::insert_into(user_mfa::table)
        .values((
            user_mfa::user_id.eq(user.id),
            user_mfa::totp_secret.eq(&secret),
        ))
        .on_conflict(user_mfa::user_id)
        .do_update()
        .set((
            user_mfa::totp_secret.eq(&secret),
            user_mfa::confirmed_at.eq(None::<chrono::DateTime<Utc>>),
        ))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(SetupBody {
        otpauth_uri: totp::otpauth_uri(&issuer(&state), &user.email, &secret),
        secret,
    }))
}

async fn qr_svg(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let pending: Option<String> = user_mfa::table
        .filter(user_mfa::user_id.eq(user.id))
        .filter(user_mfa::confirmed_at.is_null())
        .select(user_mfa::totp_secret)
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;
    let Some(secret) = pending else {
        return Err(ApiError::not_found());
    };

    let uri = totp::otpauth_uri(&issuer(&state), &user.email, &secret);
    let code = qrcode::QrCode::new(uri.as_bytes()).map_err(log_internal)?;
    let svg = code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .build();

    Ok(([(CONTENT_TYPE, "image/svg+xml")], svg).into_response())
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

#[derive(Serialize)]
struct RecoveryCodesBody {
    /// Shown exactly once; only hashes are stored.
    recovery_codes: Vec<String>,
}

async fn confirm(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CodeBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let pending: Option<String> = user_mfa::table
        .filter(user_mfa::user_id.eq(user.id))
        .filter(user_mfa::confirmed_at.is_null())
        .select(user_mfa::totp_secret)
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;
    let Some(secret) = pending else {
        return Err(ApiError::validation(
            "Start enrollment first, then confirm with a code from your app.",
        ));
    };

    if !totp::verify(&secret, &body.code, unix_now()) {
        return Err(ApiError::unauthorized(
            "That code didn't match. Check your authenticator app and try again.",
        ));
    }

    diesel::update(user_mfa::table.find(user.id))
        .set(user_mfa::confirmed_at.eq(Utc::now()))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    let codes = replace_recovery_codes(&mut connection, user.id)
        .await
        .map_err(log_internal)?;

    Ok(Json(RecoveryCodesBody {
        recovery_codes: codes,
    }))
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn disable(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<PasswordBody>,
) -> Result<impl IntoResponse, ApiError> {
    let matched = password::verify(body.password, user.password_hash.clone())
        .await
        .map_err(log_internal)?;
    if !matched {
        return Err(ApiError::unauthorized("Your password was incorrect."));
    }

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    diesel::delete(user_mfa::table.find(user.id))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;
    diesel::delete(user_recovery_codes::table.filter(user_recovery_codes::user_id.eq(user.id)))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ChallengeBody {
    mfa_token: String,
    code: String,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

/// Exchanges a login challenge plus a TOTP or recovery code for a session.
async fn verify_challenge(
    State(state): State<AuthState>,
    Json(body): Json<ChallengeBody>,
) -> Result<Response, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let peeked = user_token::peek(&mut connection, &body.mfa_token, TokenPurpose::MfaChallenge)
        .await
        .map_err(log_internal)?
        .ok_or_else(|| {
            ApiError::unauthorized("That sign-in attempt has expired. Sign in again.")
        })?;

    let accepted = code_matches(&mut connection, peeked.user_id, &body.code)
        .await
        .map_err(log_internal)?;
    if !accepted {
        user_token::record_failure(&mut connection, peeked.id)
            .await
            .map_err(log_internal)?;
        return Err(ApiError::unauthorized(
            "That code didn't match. Try again or use a recovery code.",
        ));
    }

    user_token::delete_by_id(&mut connection, peeked.id)
        .await
        .map_err(log_internal)?;

    let user: crate::auth::User = users::table
        .find(peeked.user_id)
        .select(crate::auth::User::as_select())
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    let jar = crate::auth::routes::signed_in_jar(&state, &mut connection, user.id).await?;
    let response_body = UserBody {
        user: UserResponse::from(&user),
    };
    Ok((jar, (StatusCode::OK, Json(response_body))).into_response())
}

/// True when the code is a valid TOTP or an unused recovery code.
async fn code_matches(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    submitted: &str,
) -> Result<bool, diesel::result::Error> {
    if let Some(secret) = confirmed_secret(connection, user_id).await?
        && totp::verify(&secret, submitted, unix_now())
    {
        return Ok(true);
    }

    // Recovery codes: normalize the human-friendly form before hashing.
    let normalized = submitted
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    if normalized.is_empty() {
        return Ok(false);
    }
    let code_hash = token::hash(&normalized);

    let marked = diesel::update(
        user_recovery_codes::table
            .filter(user_recovery_codes::user_id.eq(user_id))
            .filter(user_recovery_codes::code_hash.eq(&code_hash))
            .filter(user_recovery_codes::used_at.is_null()),
    )
    .set(user_recovery_codes::used_at.eq(Utc::now()))
    .execute(connection)
    .await?;

    Ok(marked > 0)
}

/// The user's confirmed TOTP secret, if two-factor auth is active.
pub(crate) async fn confirmed_secret(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<Option<String>, diesel::result::Error> {
    user_mfa::table
        .filter(user_mfa::user_id.eq(user_id))
        .filter(user_mfa::confirmed_at.is_not_null())
        .select(user_mfa::totp_secret)
        .first(connection)
        .await
        .optional()
}

/// Replaces the user's recovery codes, returning the new plaintext set.
async fn replace_recovery_codes(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<Vec<String>, diesel::result::Error> {
    diesel::delete(user_recovery_codes::table.filter(user_recovery_codes::user_id.eq(user_id)))
        .execute(connection)
        .await?;

    let mut plaintext = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _index in 0..RECOVERY_CODE_COUNT {
        let mut bytes = [0u8; 5];
        getrandom::fill(&mut bytes).expect("the OS random source must be available");
        let raw = BASE32_NOPAD.encode(&bytes);
        let code_hash = token::hash(&raw);

        diesel::insert_into(user_recovery_codes::table)
            .values((
                user_recovery_codes::user_id.eq(user_id),
                user_recovery_codes::code_hash.eq(&code_hash),
            ))
            .execute(connection)
            .await?;

        // Present as XXXX-XXXX for humans; verification strips the dash.
        plaintext.push(format!("{}-{}", &raw[0..4], &raw[4..8]));
    }

    Ok(plaintext)
}

fn issuer(state: &AuthState) -> String {
    // The app_url host names the account in authenticator apps.
    state
        .app_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split([':', '/']).next())
        .filter(|host| !host.is_empty())
        .unwrap_or("Anubis")
        .to_owned()
}

fn unix_now() -> u64 {
    // Time since the epoch never goes negative on a sane clock.
    u64::try_from(Utc::now().timestamp()).unwrap_or_default()
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "mfa request failed: {{error.message}}",
    );
    ApiError::internal()
}
