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
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};

use crate::auth::SignInMethod;
use crate::auth::model::{User, UserResponse};
use crate::auth::routes::{AuthState, signed_in_jar, validate_email};
use crate::auth::session::Sessions;
use crate::auth::user_token::TokenPurpose;
use crate::auth::{mfa, user_token};
use crate::http::ApiError;
use crate::mail::{Email, EmailKind};
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

        let mail = Email::new(
            EmailKind::SignInCode,
            user.email.clone(),
            "Your sign-in code",
            format!(
                "Your sign-in code is {code}\n\n\
                 It expires in 10 minutes. If this wasn't you, ignore this email.",
            ),
        )
        .with_param("code", &code)
        .with_param("minutes", "10");
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

/// What a submitted code led to, decided inside one transaction.
enum CodeOutcome {
    /// The code did not match, or no code was live.
    Refused,
    /// The code matched an account with a confirmed second factor, which is
    /// asked for next with this challenge token.
    SecondFactor(String),
    /// The code matched and a session was created.
    SignedIn(Box<(User, axum_extra::extract::CookieJar)>),
}

async fn verify_code(
    State(state): State<AuthState>,
    sessions: Sessions,
    context: crate::audit::Context,
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

    // Spending the code and signing in are one unit: when the session store
    // fails, the code is not spent. A wrong code commits, because the attempt
    // it costs is what keeps a six-digit code from being guessed.
    let outcome = connection
        .transaction::<_, ApiError, _>(async |transaction| {
            let accepted = user_token::verify_short_code(
                transaction,
                user.id,
                TokenPurpose::EmailSignIn,
                &body.code,
            )
            .await?;
            if !accepted {
                return Ok(CodeOutcome::Refused);
            }

            // Proving inbox control also proves the address; mark it verified.
            let user = if user.email_verified_at.is_none() {
                diesel::update(users::table.find(user.id))
                    .set(users::email_verified_at.eq(chrono::Utc::now()))
                    .returning(User::as_returning())
                    .get_result(transaction)
                    .await?
            } else {
                user.clone()
            };

            // An inbox alone never bypasses a confirmed second factor.
            if mfa::confirmed_secret(transaction, &state.secret_key, user.id)
                .await?
                .is_some()
            {
                let mfa_token =
                    user_token::issue(transaction, user.id, TokenPurpose::MfaChallenge).await?;
                return Ok(CodeOutcome::SecondFactor(mfa_token));
            }

            let jar = signed_in_jar(
                &state,
                &sessions,
                transaction,
                &context,
                &user,
                SignInMethod::EmailCode,
            )
            .await?;
            Ok(CodeOutcome::SignedIn(Box::new((user, jar))))
        })
        .await?;

    let (user, jar) = match outcome {
        CodeOutcome::Refused => return Err(invalid_code()),
        CodeOutcome::SecondFactor(mfa_token) => {
            return Ok((
                StatusCode::OK,
                Json(MfaChallengeBody {
                    mfa_required: true,
                    mfa_token,
                }),
            )
                .into_response());
        }
        CodeOutcome::SignedIn(signed_in) => *signed_in,
    };

    let response_body = UserBody {
        user: UserResponse::load(&mut connection, &user)
            .await
            .map_err(log_internal)?,
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
