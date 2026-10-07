//! The invitee's half of an operator's invitation: reading it and accepting it.
//!
//! These routes merge into the auth router (conventionally under `/auth`):
//!
//! | Route | Effect |
//! |---|---|
//! | `POST /invitations/lookup` | Answer the address a live invitation was sent to |
//! | `POST /invitations/accept` | Set the first password, create the account, sign in |
//!
//! Both carry the token in the body rather than the path, as every other
//! emailed token does, because a path is what request tracing writes down.
//! Both answer an unknown, used, revoked, and expired token with the same
//! `400` and the same sentence, so neither tells a caller which it was, and
//! both spend the credential-guessing budget, because a token is a credential.
//!
//! The operator's half, sending and managing invitations, is
//! [`crate::platform::Accounts`].

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use diesel_async::AsyncConnection;
use serde::{Deserialize, Serialize};

use crate::audit;
use crate::auth::model::UserResponse;
use crate::auth::routes::{AuthState, registration_preference, signed_in_jar, validate_password};
use crate::auth::session::SignInMethod;
use crate::http::ApiError;
use crate::platform::{Acceptance, INVITATION_UNUSABLE, accept_invitation, live_invitation};
use crate::rate_limit::{Budget, RateLimiter};

pub(crate) fn router(rate_limit: &RateLimiter) -> Router<AuthState> {
    Router::new()
        .route(
            "/invitations/lookup",
            post(lookup_invitation).layer(rate_limit.layer(Budget::Credentials)),
        )
        .route(
            "/invitations/accept",
            post(accept).layer(rate_limit.layer(Budget::Credentials)),
        )
}

#[derive(Deserialize)]
struct LookupBody {
    token: String,
}

/// What the accept page shows before the invitee chooses a password.
#[derive(Serialize)]
struct InvitationBody {
    email: String,
    expires_at: DateTime<Utc>,
}

async fn lookup_invitation(
    State(state): State<AuthState>,
    Json(body): Json<LookupBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let invitation = live_invitation(&mut connection, &body.token)
        .await?
        .ok_or_else(|| ApiError::validation(INVITATION_UNUSABLE))?;

    Ok(Json(InvitationBody {
        email: invitation.email,
        expires_at: invitation.expires_at,
    }))
}

/// What accepting carries: the token, the first password, and the
/// preferences the browser already knows, exactly as a sign-up does.
#[derive(Deserialize)]
struct AcceptBody {
    token: String,
    password: String,
    time_zone: Option<String>,
    locale: Option<String>,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

async fn accept(
    State(state): State<AuthState>,
    context: audit::Context,
    Json(body): Json<AcceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate_password(&body.password)?;
    let time_zone = registration_preference(body.time_zone, "time zone");
    let locale = registration_preference(body.locale, "locale");

    // Hashing is the slow part and needs no transaction, so it happens before
    // one is opened rather than while a row is held.
    let password_hash = state.hasher.hash(body.password).await?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // The account, the acceptance, and the session commit together: an
    // invitee is never left with an account the link can no longer reach and
    // no session to show for it.
    let (user, jar) = connection
        .transaction::<_, ApiError, _>(async |transaction| {
            let user = accept_invitation(
                transaction,
                &context,
                &body.token,
                Acceptance {
                    password_hash: &password_hash,
                    time_zone: time_zone.as_deref(),
                    locale: locale.as_deref(),
                },
            )
            .await?;
            let jar = signed_in_jar(
                &state,
                transaction,
                &context,
                &user,
                SignInMethod::Invitation,
            )
            .await?;
            Ok((user, jar))
        })
        .await?;

    let body = UserBody {
        user: UserResponse::load(&mut connection, &user)
            .await
            .map_err(log_internal)?,
    };
    Ok((jar, (StatusCode::CREATED, Json(body))))
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "invitation request failed: {{error.message}}",
    );
    ApiError::internal()
}
