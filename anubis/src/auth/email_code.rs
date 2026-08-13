//! Passwordless sign-in with emailed one-time codes.
//!
//! `POST /email-code/request` always answers the same 202, whether or not
//! the email is registered, and only mails real accounts, so responses
//! cannot probe which addresses exist. `POST /email-code/verify` exchanges
//! email + code for a session; codes live ten minutes, die after a handful
//! of wrong attempts, and are single-use. An account with a confirmed second
//! factor still gets the MFA challenge: an inbox alone never bypasses TOTP.
//!
//! Both routes carry a per-client budget from [`crate::rate_limit`], and the
//! request route also charges the address it would mail, so rotating client
//! addresses cannot bomb one inbox.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};

use crate::auth::model::{User, UserResponse};
use crate::auth::routes::{AuthState, signed_in_jar, validate_email};
use crate::auth::user_token::TokenPurpose;
use crate::auth::{mfa, user_token};
use crate::http::ApiError;
use crate::mail::Email;
use crate::rate_limit::{Budget, RateLimiter};
use crate::schema::users;

pub(crate) fn router(rate_limit: &RateLimiter) -> Router<AuthState> {
    Router::new()
        .route(
            "/email-code/request",
            post(request_code).layer(rate_limit.layer(Budget::EmailPerClient)),
        )
        .route(
            "/email-code/verify",
            post(verify_code).layer(rate_limit.layer(Budget::Credentials)),
        )
}

#[derive(Deserialize)]
struct RequestBody {
    email: String,
}

#[derive(Serialize)]
struct MessageBody {
    message: &'static str,
}

async fn request_code(
    State(state): State<AuthState>,
    Json(body): Json<RequestBody>,
) -> Result<impl IntoResponse, ApiError> {
    let email = validate_email(&body.email)?;

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
        let code =
            user_token::issue_short_code(&mut connection, user.id, TokenPurpose::EmailSignIn)
                .await
                .map_err(log_internal)?;

        let mail = Email {
            to: user.email.clone(),
            subject: "Your sign-in code".to_owned(),
            text_body: format!(
                "Your sign-in code is {code}\n\n\
                 It expires in 10 minutes. If this wasn't you, ignore this email.",
            ),
        };
        if let Err(error) = state.mailer.send(mail).await {
            tracing::error!(
                error.message = %error,
                "failed to deliver the sign-in code: {{error.message}}",
            );
        }
    }

    // Unknown emails answer identically, so responses cannot probe accounts.
    Ok((
        StatusCode::ACCEPTED,
        Json(MessageBody {
            message: "If that email is registered, a sign-in code is on its way.",
        }),
    ))
}

#[derive(Deserialize)]
struct VerifyBody {
    email: String,
    code: String,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

#[derive(Serialize)]
struct MfaChallengeBody {
    mfa_required: bool,
    mfa_token: String,
}

async fn verify_code(
    State(state): State<AuthState>,
    Json(body): Json<VerifyBody>,
) -> Result<Response, ApiError> {
    let email = validate_email(&body.email)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let user: Option<User> = users::table
        .filter(users::email.eq(&email))
        .select(User::as_select())
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;

    // Unknown emails and wrong codes reject identically.
    let Some(user) = user else {
        return Err(invalid_code());
    };

    let accepted = user_token::verify_short_code(
        &mut connection,
        user.id,
        TokenPurpose::EmailSignIn,
        &body.code,
    )
    .await
    .map_err(log_internal)?;
    if !accepted {
        return Err(invalid_code());
    }

    // Proving inbox control also proves the address; mark it verified.
    let user = if user.email_verified_at.is_none() {
        diesel::update(users::table.find(user.id))
            .set(users::email_verified_at.eq(chrono::Utc::now()))
            .returning(User::as_returning())
            .get_result(&mut connection)
            .await
            .map_err(log_internal)?
    } else {
        user
    };

    // An inbox alone never bypasses a confirmed second factor.
    if mfa::confirmed_secret(&mut connection, &state.secret_key, user.id)
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
    let response_body = UserBody {
        user: UserResponse::from(&user),
    };
    Ok((jar, (StatusCode::OK, Json(response_body))).into_response())
}

fn invalid_code() -> ApiError {
    ApiError::unauthorized("That code didn't match or has expired. Request a new one.")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "email-code request failed: {{error.message}}",
    );
    ApiError::internal()
}
