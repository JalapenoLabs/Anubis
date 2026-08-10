//! Passkeys: WebAuthn credential registration and username-less login.
//!
//! Registration happens signed-in: `POST /passkeys/register/start` returns
//! the browser's creation options plus an opaque state token, and
//! `POST /passkeys/register/finish` stores the attested credential. Login is
//! the discoverable-credential flow password managers implement:
//! `POST /passkeys/login/start` needs no username, and
//! `POST /passkeys/login/finish` verifies the assertion and issues the
//! session. A passkey is multi-factor by construction, so passkey login
//! bypasses the TOTP challenge.
//!
//! Cross-platform authenticators are welcome; nothing here requires a
//! platform authenticator, which is what keeps password-manager passkeys
//! (Bitwarden and friends) first-class.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use webauthn_rs::prelude::{
    CreationChallengeResponse, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Webauthn, WebauthnBuilder,
};

use crate::auth::model::{User, UserResponse};
use crate::auth::routes::{AuthState, signed_in_jar};
use crate::auth::{CurrentUser, token};
use crate::http::ApiError;
use crate::schema::{user_passkeys, users, webauthn_states};

/// How long a started ceremony may wait for its finish call.
const CEREMONY_TTL_MINUTES: i64 = 5;

/// The `purpose` column values for ceremony state rows.
const PURPOSE_REGISTRATION: &str = "registration";
const PURPOSE_AUTHENTICATION: &str = "authentication";

pub(crate) fn router() -> Router<AuthState> {
    Router::new()
        .route("/passkeys", get(list_passkeys))
        .route("/passkeys/{passkey_id}", delete(remove_passkey))
        .route("/passkeys/register/start", post(register_start))
        .route("/passkeys/register/finish", post(register_finish))
        .route("/passkeys/login/start", post(login_start))
        .route("/passkeys/login/finish", post(login_finish))
}

/// Builds the WebAuthn relying party from the configured public URL.
fn relying_party(state: &AuthState) -> Result<Webauthn, ApiError> {
    let origin = url::Url::parse(&state.app_url).map_err(log_internal)?;
    let rp_id = origin.host_str().ok_or_else(|| {
        tracing::error!("APP_URL has no host; passkeys need a real origin");
        ApiError::internal()
    })?;

    WebauthnBuilder::new(rp_id, &origin)
        .map_err(log_internal)?
        .rp_name(rp_id)
        .build()
        .map_err(log_internal)
}

#[derive(Serialize)]
struct PasskeyEntry {
    id: Uuid,
    name: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct PasskeysBody {
    passkeys: Vec<PasskeyEntry>,
}

/// One listed passkey row: id, name, created, last used.
type PasskeyRow = (Uuid, String, DateTime<Utc>, Option<DateTime<Utc>>);

async fn list_passkeys(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let rows: Vec<PasskeyRow> = user_passkeys::table
        .filter(user_passkeys::user_id.eq(user.id))
        .select((
            user_passkeys::id,
            user_passkeys::name,
            user_passkeys::created_at,
            user_passkeys::last_used_at,
        ))
        .order(user_passkeys::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    let passkeys = rows
        .into_iter()
        .map(|(id, name, created_at, last_used_at)| PasskeyEntry {
            id,
            name,
            created_at,
            last_used_at,
        })
        .collect();

    Ok(Json(PasskeysBody { passkeys }))
}

async fn remove_passkey(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Path(passkey_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let deleted = diesel::delete(
        user_passkeys::table
            .filter(user_passkeys::id.eq(passkey_id))
            .filter(user_passkeys::user_id.eq(user.id)),
    )
    .execute(&mut connection)
    .await
    .map_err(log_internal)?;

    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct RegisterStartBody {
    /// Present with the finish call.
    state_token: String,
    /// Feed to `navigator.credentials.create` as `publicKey` options.
    creation_options: CreationChallengeResponse,
}

async fn register_start(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let webauthn = relying_party(&state)?;
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let existing = load_passkeys(&mut connection, user.id).await?;
    let exclude = existing
        .iter()
        .map(|(_row_id, passkey)| passkey.cred_id().clone())
        .collect::<Vec<_>>();
    let exclude = if exclude.is_empty() {
        None
    } else {
        Some(exclude)
    };

    let display_name = user
        .first_name
        .clone()
        .unwrap_or_else(|| user.email.clone());
    let (creation_options, registration_state) = webauthn
        .start_passkey_registration(user.id, &user.email, &display_name, exclude)
        .map_err(log_internal)?;

    let state_token = store_state(
        &mut connection,
        PURPOSE_REGISTRATION,
        Some(user.id),
        &registration_state,
    )
    .await?;

    Ok(Json(RegisterStartBody {
        state_token,
        creation_options,
    }))
}

#[derive(Deserialize)]
struct RegisterFinishBody {
    state_token: String,
    /// The browser's `navigator.credentials.create` result.
    credential: RegisterPublicKeyCredential,
    name: Option<String>,
}

async fn register_finish(
    State(state): State<AuthState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<RegisterFinishBody>,
) -> Result<impl IntoResponse, ApiError> {
    let webauthn = relying_party(&state)?;
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (state_user, registration_state): (Option<Uuid>, PasskeyRegistration) =
        take_state(&mut connection, PURPOSE_REGISTRATION, &body.state_token).await?;
    if state_user != Some(user.id) {
        return Err(expired_ceremony());
    }

    let passkey = webauthn
        .finish_passkey_registration(&body.credential, &registration_state)
        .map_err(|error| {
            tracing::debug!(
                error.message = %error,
                "passkey registration rejected: {{error.message}}",
            );
            ApiError::validation("That passkey could not be verified. Try registering again.")
        })?;

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Passkey")
        .chars()
        .take(100)
        .collect::<String>();
    let credential_json = serde_json::to_value(&passkey).map_err(log_internal)?;

    let created: (Uuid, DateTime<Utc>) = diesel::insert_into(user_passkeys::table)
        .values((
            user_passkeys::user_id.eq(user.id),
            user_passkeys::name.eq(&name),
            user_passkeys::credential.eq(&credential_json),
        ))
        .returning((user_passkeys::id, user_passkeys::created_at))
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok((
        StatusCode::CREATED,
        Json(PasskeysBody {
            passkeys: vec![PasskeyEntry {
                id: created.0,
                name,
                created_at: created.1,
                last_used_at: None,
            }],
        }),
    ))
}

#[derive(Serialize)]
struct LoginStartBody {
    /// Present with the finish call.
    state_token: String,
    /// Feed to `navigator.credentials.get` as `publicKey` options.
    request_options: RequestChallengeResponse,
}

async fn login_start(State(state): State<AuthState>) -> Result<impl IntoResponse, ApiError> {
    let webauthn = relying_party(&state)?;
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (request_options, authentication_state) = webauthn
        .start_discoverable_authentication()
        .map_err(log_internal)?;

    let state_token = store_state(
        &mut connection,
        PURPOSE_AUTHENTICATION,
        None,
        &authentication_state,
    )
    .await?;

    Ok(Json(LoginStartBody {
        state_token,
        request_options,
    }))
}

#[derive(Deserialize)]
struct LoginFinishBody {
    state_token: String,
    /// The browser's `navigator.credentials.get` result.
    credential: PublicKeyCredential,
}

#[derive(Serialize)]
struct UserBody {
    user: UserResponse,
}

async fn login_finish(
    State(state): State<AuthState>,
    Json(body): Json<LoginFinishBody>,
) -> Result<impl IntoResponse, ApiError> {
    let webauthn = relying_party(&state)?;
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let (_state_user, authentication_state): (Option<Uuid>, DiscoverableAuthentication) =
        take_state(&mut connection, PURPOSE_AUTHENTICATION, &body.state_token).await?;

    // The discoverable credential names its user; load that user's keys.
    let (user_id, _credential_id) = webauthn
        .identify_discoverable_authentication(&body.credential)
        .map_err(|_error| invalid_passkey())?;

    let stored = load_passkeys(&mut connection, user_id).await?;
    if stored.is_empty() {
        return Err(invalid_passkey());
    }
    let discoverable: Vec<DiscoverableKey> = stored
        .iter()
        .map(|(_row_id, passkey)| DiscoverableKey::from(passkey))
        .collect();

    let result = webauthn
        .finish_discoverable_authentication(&body.credential, authentication_state, &discoverable)
        .map_err(|error| {
            tracing::debug!(
                error.message = %error,
                "passkey login rejected: {{error.message}}",
            );
            invalid_passkey()
        })?;

    // Persist any counter/backup-state updates on the used credential.
    for (row_id, mut passkey) in stored {
        if passkey.cred_id() == result.cred_id() {
            passkey.update_credential(&result);
            let updated_json = serde_json::to_value(&passkey).map_err(log_internal)?;
            diesel::update(user_passkeys::table.find(row_id))
                .set((
                    user_passkeys::credential.eq(&updated_json),
                    user_passkeys::last_used_at.eq(Utc::now()),
                ))
                .execute(&mut connection)
                .await
                .map_err(log_internal)?;
        }
    }

    let user: User = users::table
        .find(user_id)
        .select(User::as_select())
        .first(&mut connection)
        .await
        .map_err(log_internal)?;

    // A passkey is possession plus verification: strong auth, no TOTP step.
    let jar = signed_in_jar(&state, &mut connection, user.id).await?;
    let response_body = UserBody {
        user: UserResponse::from(&user),
    };
    Ok((jar, (StatusCode::OK, Json(response_body))))
}

/// Loads a user's passkeys with their row ids, skipping rows that no longer
/// deserialize.
async fn load_passkeys(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<Vec<(Uuid, Passkey)>, ApiError> {
    let rows: Vec<(Uuid, serde_json::Value)> = user_passkeys::table
        .filter(user_passkeys::user_id.eq(user_id))
        .select((user_passkeys::id, user_passkeys::credential))
        .load(connection)
        .await
        .map_err(log_internal)?;

    Ok(rows
        .into_iter()
        .filter_map(|(row_id, value)| {
            serde_json::from_value(value)
                .ok()
                .map(|passkey| (row_id, passkey))
        })
        .collect())
}

/// Stores serialized ceremony state, returning the client's state token.
async fn store_state<StateData: serde::Serialize>(
    connection: &mut AsyncPgConnection,
    purpose: &str,
    user_id: Option<Uuid>,
    state_data: &StateData,
) -> Result<String, ApiError> {
    // Opportunistically sweep expired ceremonies.
    diesel::delete(webauthn_states::table.filter(webauthn_states::expires_at.le(Utc::now())))
        .execute(connection)
        .await
        .map_err(log_internal)?;

    let raw_token = token::generate();
    let token_hash = token::hash(&raw_token);
    let state_json = serde_json::to_value(state_data).map_err(log_internal)?;

    diesel::insert_into(webauthn_states::table)
        .values((
            webauthn_states::purpose.eq(purpose),
            webauthn_states::user_id.eq(user_id),
            webauthn_states::token_hash.eq(&token_hash),
            webauthn_states::state.eq(&state_json),
            webauthn_states::expires_at.eq(Utc::now() + Duration::minutes(CEREMONY_TTL_MINUTES)),
        ))
        .execute(connection)
        .await
        .map_err(log_internal)?;

    Ok(raw_token)
}

/// Consumes ceremony state by token; a state token is single-use.
async fn take_state<StateData: serde::de::DeserializeOwned>(
    connection: &mut AsyncPgConnection,
    purpose: &str,
    raw_token: &str,
) -> Result<(Option<Uuid>, StateData), ApiError> {
    let token_hash = token::hash(raw_token);

    let row: Option<(Option<Uuid>, serde_json::Value)> = diesel::delete(
        webauthn_states::table
            .filter(webauthn_states::token_hash.eq(&token_hash))
            .filter(webauthn_states::purpose.eq(purpose))
            .filter(webauthn_states::expires_at.gt(Utc::now())),
    )
    .returning((webauthn_states::user_id, webauthn_states::state))
    .get_result(connection)
    .await
    .optional()
    .map_err(log_internal)?;

    let Some((user_id, state_json)) = row else {
        return Err(expired_ceremony());
    };
    let state_data = serde_json::from_value(state_json).map_err(log_internal)?;
    Ok((user_id, state_data))
}

fn expired_ceremony() -> ApiError {
    ApiError::validation("That passkey request has expired. Start again.")
}

fn invalid_passkey() -> ApiError {
    ApiError::unauthorized("That passkey was not recognized.")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "passkey request failed: {{error.message}}",
    );
    ApiError::internal()
}
