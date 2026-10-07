//! The token endpoint and the revocation endpoint.
//!
//! Both take `application/x-www-form-urlencoded` bodies, which RFC 6749 and RFC
//! 7009 require and which Claude sends; a JSON-only endpoint would refuse every
//! real client. Clients are public, so a client authenticates by naming its
//! `client_id` and by holding what only it could hold: the PKCE verifier for a
//! code, and the current refresh token for a refresh.
//!
//! # Spending a code or a refresh token
//!
//! Both are spent with one conditional update, `SET used_at = now() WHERE
//! used_at IS NULL`, never a read followed by a write. Two exchanges racing
//! for one code cannot both win, and the loser learns it lost from the same
//! statement. Losing is also how theft looks: a code or a refresh token
//! presented after it was spent means two parties hold it, and OAuth 2.1 says
//! the grant behind it must not be trusted any further. So the second
//! presentation revokes the whole grant and records why
//! ([`audit::OAUTH_CODE_REUSED`], [`audit::OAUTH_REFRESH_REUSED`]).
//!
//! The consequence for a client is that two refreshes sent at the same moment
//! with the same token end its connection. That is the trade the specification
//! makes, and a client that serializes its refreshes never meets it.

use axum::Json;
use axum::extract::rejection::FormRejection;
use axum::extract::{Form, State};
use axum::http::header::CACHE_CONTROL;
use axum::response::IntoResponse;
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit;
use crate::auth::token;
use crate::oauth_server::error::OauthError;
use crate::oauth_server::grant::{self, ACCESS_TOKEN_TTL, Grant, IssuedTokens};
use crate::oauth_server::{Server, pkce};
use crate::schema::{
    oauth_access_tokens, oauth_authorization_codes, oauth_clients, oauth_grants,
    oauth_refresh_tokens,
};

/// A token request. Every field is optional so a missing one is answered as
/// `invalid_request` in OAuth's shape rather than as a framework rejection.
#[derive(Debug, Deserialize)]
pub(crate) struct TokenForm {
    grant_type: Option<String>,
    client_id: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
    scope: Option<String>,
}

/// A successful token response, RFC 6749 section 5.1.
#[derive(Debug, Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    refresh_token: String,
    scope: String,
}

impl From<IssuedTokens> for TokenResponse {
    fn from(issued: IssuedTokens) -> Self {
        Self {
            access_token: issued.access_token,
            token_type: "Bearer",
            expires_in: ACCESS_TOKEN_TTL.num_seconds(),
            refresh_token: issued.refresh_token,
            scope: issued.scopes.join(" "),
        }
    }
}

/// `POST /oauth/token`.
pub(crate) async fn exchange(
    State(server): State<Server>,
    context: audit::Context,
    form: Result<Form<TokenForm>, FormRejection>,
) -> Result<impl IntoResponse, OauthError> {
    let Form(form) = form.map_err(|rejection| {
        OauthError::invalid_request(format!(
            "The token request must be a form-encoded body: {rejection}"
        ))
    })?;
    let client_id = form
        .client_id
        .as_deref()
        .ok_or_else(|| OauthError::invalid_request("client_id is required."))?;
    if form
        .resource
        .as_deref()
        .is_some_and(|resource| !server.is_resource(resource))
    {
        return Err(OauthError::invalid_target(
            "The resource is not one this server issues tokens for.",
        ));
    }

    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "the token endpoint could not reach the database: {{error.message}}",
        );
        OauthError::server_error()
    })?;

    // The transaction commits whether the exchange succeeds or not: a spent
    // code stays spent, and a reuse revokes its grant, even though the answer
    // is a refusal. Outcomes are values here, never the transaction's error.
    let outcome = connection
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            match form.grant_type.as_deref() {
                Some("authorization_code") => {
                    exchange_code(transaction, &context, client_id, &form).await
                }
                Some("refresh_token") => refresh(transaction, &context, client_id, &form).await,
                _ => Ok(Err(OauthError::unsupported_grant_type())),
            }
        })
        .await??;

    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(TokenResponse::from(outcome)),
    ))
}

/// What one exchange produced: tokens, or the refusal to answer with.
type Outcome = Result<IssuedTokens, OauthError>;

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = oauth_authorization_codes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SpentCode {
    grant_id: Uuid,
    redirect_uri: String,
    code_challenge: String,
    expires_at: chrono::DateTime<Utc>,
}

/// The `authorization_code` grant.
async fn exchange_code(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    client_id: &str,
    form: &TokenForm,
) -> QueryResult<Outcome> {
    let (Some(code), Some(redirect_uri), Some(verifier)) = (
        form.code.as_deref(),
        form.redirect_uri.as_deref(),
        form.code_verifier.as_deref(),
    ) else {
        return Ok(Err(OauthError::invalid_request(
            "code, redirect_uri, and code_verifier are required.",
        )));
    };
    let code_hash = token::hash(code);

    let spent: Option<SpentCode> = diesel::update(
        oauth_authorization_codes::table
            .filter(oauth_authorization_codes::code_hash.eq(&code_hash))
            .filter(oauth_authorization_codes::used_at.is_null()),
    )
    .set(oauth_authorization_codes::used_at.eq(Some(Utc::now())))
    .returning(SpentCode::as_returning())
    .get_result(connection)
    .await
    .optional()?;

    let Some(spent) = spent else {
        // Spent already, or never issued. A code that exists was replayed.
        let replayed: Option<Uuid> = oauth_authorization_codes::table
            .filter(oauth_authorization_codes::code_hash.eq(&code_hash))
            .select(oauth_authorization_codes::grant_id)
            .first(connection)
            .await
            .optional()?;
        if let Some(grant_id) = replayed {
            revoke_for_reuse(connection, context, grant_id, audit::OAUTH_CODE_REUSED).await?;
        }
        return Ok(Err(OauthError::invalid_grant(
            "The authorization code is invalid or was already used.",
        )));
    };

    let Some((grant, owner)) = live_grant_with_client(connection, spent.grant_id).await? else {
        return Ok(Err(OauthError::invalid_grant(
            "The authorization code is invalid or was already used.",
        )));
    };
    // The client named here must be the one the code was issued to, the code
    // must still be fresh, the redirect must be the one it was delivered to
    // (RFC 6749 section 4.1.3), and the verifier must answer the challenge.
    if owner != client_id || spent.expires_at <= Utc::now() || spent.redirect_uri != redirect_uri {
        return Ok(Err(OauthError::invalid_grant(
            "The authorization code is invalid or was already used.",
        )));
    }
    if !pkce::verify(verifier, &spent.code_challenge) {
        return Ok(Err(OauthError::invalid_grant(
            "The code verifier does not match the code challenge.",
        )));
    }

    let scopes = grant.scopes.clone();
    Ok(Ok(grant::issue_tokens(connection, &grant, scopes).await?))
}

/// The `refresh_token` grant, which rotates the refresh token it spends.
async fn refresh(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    client_id: &str,
    form: &TokenForm,
) -> QueryResult<Outcome> {
    let Some(presented) = form.refresh_token.as_deref() else {
        return Ok(Err(OauthError::invalid_request(
            "refresh_token is required.",
        )));
    };
    let token_hash = token::hash(presented);

    // A refresh may ask for less than the grant holds, never more. Checked
    // before the token is spent, so a client that asked for too much keeps the
    // token it can retry with; the grant's scopes never change, so reading
    // them first decides nothing the spend below could contradict. Only a
    // token that could still be spent is read here: a spent, expired, or
    // revoked one falls through to the spend below, which is where reuse is
    // detected, so no scope a caller names can skip that check or learn
    // that a dead token was ever real.
    if let Some(requested) = form.scope.as_deref() {
        let held: Option<Vec<String>> = oauth_refresh_tokens::table
            .inner_join(oauth_grants::table)
            .filter(oauth_refresh_tokens::token_hash.eq(&token_hash))
            .filter(oauth_refresh_tokens::used_at.is_null())
            .filter(oauth_refresh_tokens::expires_at.gt(Utc::now()))
            .filter(oauth_grants::revoked_at.is_null())
            .select(oauth_grants::scopes)
            .first(connection)
            .await
            .optional()?;
        let asks_for_more = held.is_some_and(|held| {
            requested
                .split_ascii_whitespace()
                .any(|name| !held.iter().any(|scope| scope == name))
        });
        if asks_for_more {
            return Ok(Err(OauthError::invalid_scope(
                "A refresh cannot ask for a scope the grant does not hold.",
            )));
        }
    }

    let spent: Option<Uuid> = diesel::update(
        oauth_refresh_tokens::table
            .filter(oauth_refresh_tokens::token_hash.eq(&token_hash))
            .filter(oauth_refresh_tokens::used_at.is_null())
            .filter(oauth_refresh_tokens::expires_at.gt(Utc::now())),
    )
    .set(oauth_refresh_tokens::used_at.eq(Some(Utc::now())))
    .returning(oauth_refresh_tokens::grant_id)
    .get_result(connection)
    .await
    .optional()?;

    let Some(grant_id) = spent else {
        let rotated_away: Option<Uuid> = oauth_refresh_tokens::table
            .filter(oauth_refresh_tokens::token_hash.eq(&token_hash))
            .filter(oauth_refresh_tokens::used_at.is_not_null())
            .select(oauth_refresh_tokens::grant_id)
            .first(connection)
            .await
            .optional()?;
        if let Some(grant_id) = rotated_away {
            revoke_for_reuse(connection, context, grant_id, audit::OAUTH_REFRESH_REUSED).await?;
        }
        return Ok(Err(OauthError::invalid_grant(
            "The refresh token is invalid, expired, or was already used.",
        )));
    };

    let Some((grant, owner)) = live_grant_with_client(connection, grant_id).await? else {
        return Ok(Err(OauthError::invalid_grant(
            "The refresh token is invalid, expired, or was already used.",
        )));
    };
    if owner != client_id {
        return Ok(Err(OauthError::invalid_grant(
            "The refresh token is invalid, expired, or was already used.",
        )));
    }

    let scopes = match form.scope.as_deref() {
        None => grant.scopes.clone(),
        Some(requested) => grant
            .scopes
            .iter()
            .filter(|held| requested.split_ascii_whitespace().any(|name| name == *held))
            .cloned()
            .collect(),
    };

    Ok(Ok(grant::issue_tokens(connection, &grant, scopes).await?))
}

/// A live grant and the `client_id` string of the client it belongs to.
async fn live_grant_with_client(
    connection: &mut AsyncPgConnection,
    grant_id: Uuid,
) -> QueryResult<Option<(Grant, String)>> {
    oauth_grants::table
        .inner_join(oauth_clients::table)
        .filter(oauth_grants::id.eq(grant_id))
        .filter(oauth_grants::revoked_at.is_null())
        .select((Grant::as_select(), oauth_clients::client_id))
        .first(connection)
        .await
        .optional()
}

/// Revokes a grant whose code or refresh token was presented twice.
async fn revoke_for_reuse(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    grant_id: Uuid,
    action: &str,
) -> QueryResult<()> {
    let found: Option<(Grant, String)> = oauth_grants::table
        .inner_join(oauth_clients::table)
        .filter(oauth_grants::id.eq(grant_id))
        .select((Grant::as_select(), oauth_clients::name))
        .first(connection)
        .await
        .optional()?;
    let Some((reused, client_name)) = found else {
        return Ok(());
    };

    if grant::revoke(connection, grant_id).await? {
        tracing::warn!(
            oauth.grant_id = %grant_id,
            oauth.action = action,
            "a spent credential was presented again; grant {{oauth.grant_id}} is revoked",
        );
        grant::record(connection, context, action, &reused, &client_name).await?;
    }
    Ok(())
}

/// A revocation request, RFC 7009 section 2.1.
#[derive(Debug, Deserialize)]
pub(crate) struct RevokeForm {
    token: Option<String>,
    client_id: Option<String>,
}

/// `POST /oauth/revoke`.
///
/// Revoking a refresh token ends its grant, the whole connection. Revoking an
/// access token ends that token alone, since a client discarding one token is
/// not a client disconnecting. The answer is `200` whether or not the token
/// meant anything, as RFC 7009 requires, so the endpoint confirms nothing to a
/// prober. A token belonging to another client is left alone, with the same
/// `200`.
pub(crate) async fn revoke(
    State(server): State<Server>,
    context: audit::Context,
    form: Result<Form<RevokeForm>, FormRejection>,
) -> Result<impl IntoResponse, OauthError> {
    let Form(form) = form.map_err(|rejection| {
        OauthError::invalid_request(format!(
            "The revocation request must be a form-encoded body: {rejection}"
        ))
    })?;
    let (Some(presented), Some(client_id)) = (form.token.as_deref(), form.client_id.as_deref())
    else {
        return Err(OauthError::invalid_request(
            "token and client_id are required.",
        ));
    };
    let token_hash = token::hash(presented);

    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "revocation could not reach the database: {{error.message}}",
        );
        OauthError::server_error()
    })?;

    connection
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            let refresh_grant: Option<(Grant, String)> = oauth_refresh_tokens::table
                .inner_join(oauth_grants::table.inner_join(oauth_clients::table))
                .filter(oauth_refresh_tokens::token_hash.eq(&token_hash))
                .filter(oauth_clients::client_id.eq(client_id))
                .select((Grant::as_select(), oauth_clients::name))
                .first(transaction)
                .await
                .optional()?;
            if let Some((revoked, client_name)) = refresh_grant {
                if grant::revoke(transaction, revoked.id).await? {
                    grant::record(
                        transaction,
                        &context,
                        audit::OAUTH_REVOKED,
                        &revoked,
                        &client_name,
                    )
                    .await?;
                }
                return Ok(());
            }

            let owned_access_token: Option<Uuid> = oauth_access_tokens::table
                .inner_join(oauth_grants::table.inner_join(oauth_clients::table))
                .filter(oauth_access_tokens::token_hash.eq(&token_hash))
                .filter(oauth_clients::client_id.eq(client_id))
                .select(oauth_access_tokens::id)
                .first(transaction)
                .await
                .optional()?;
            if let Some(access_token_id) = owned_access_token {
                diesel::delete(
                    oauth_access_tokens::table.filter(oauth_access_tokens::id.eq(access_token_id)),
                )
                .execute(transaction)
                .await?;
            }
            Ok(())
        })
        .await?;

    Ok([(CACHE_CONTROL, "no-store")])
}
