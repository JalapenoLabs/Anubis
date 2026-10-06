//! The authorization endpoint and the consent decision behind it.
//!
//! Three routes make one ceremony:
//!
//! | Route | Who calls it | Effect |
//! |---|---|---|
//! | `GET /oauth/authorize` | the browser, sent by the client | Validate the request, store it, open the consent screen |
//! | `GET /oauth/requests/{id}` | the consent screen | What the person is being asked to approve |
//! | `POST /oauth/requests/{id}` | the consent screen | Approve or deny, answering where the browser goes next |
//!
//! The consent screen is the application's SPA at `/consent?request=<id>`, so
//! it signs a visitor in on the way with the pages it already has, and the
//! decision is a session-authenticated JSON request like every other account
//! action. Nothing the client sent rides through the browser twice: the stored
//! request is what the decision acts on.
//!
//! # Errors before and after the redirect URI is trusted
//!
//! RFC 6749 section 4.1.2.1 splits failures in two. Until the client and its
//! redirect URI check out, nothing may be sent to that URI, because sending a
//! browser to an unverified address is an open redirect; those failures land
//! on the consent screen as `/consent?error=<code>`. After that, a failure
//! goes back to the client as `error=<code>` on its redirect URI, with its
//! `state` and this server's `iss` (RFC 9207), so the client can tell which
//! request failed and which server said so.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::audit;
use crate::auth::{CurrentUser, token};
use crate::http::ApiError;
use crate::oauth_server::client::{self, ClientFailure, ClientKind, ClientRow};
use crate::oauth_server::{Server, grant, pkce, redirect};
use crate::schema::{oauth_authorization_codes, oauth_authorization_requests, oauth_clients};

/// Where the SPA renders the consent screen.
///
/// A contract with the application's frontend, the way `/sign-in` is: the
/// starter's `UrlTree.consent` names the same path, and the screen reads
/// either `request` or `error` from the query string.
pub const CONSENT_PATH: &str = "/consent";

/// How long a person has to decide.
const REQUEST_TTL_MINUTES: i64 = 10;

/// How long an authorization code waits to be exchanged.
///
/// The client exchanges it the moment the browser lands on its redirect URI,
/// so two minutes is generous; RFC 6749 recommends at most ten.
const CODE_TTL_MINUTES: i64 = 2;

/// The longest `state` this server echoes back.
///
/// It is the client's own value, stored and returned verbatim, so it is bounded
/// rather than trusted to be small.
const MAX_STATE_LENGTH: usize = 1024;

/// The authorization request, as a client sends it.
#[derive(Debug, Deserialize)]
pub(crate) struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = oauth_authorization_requests)]
struct NewAuthorizationRequest<'a> {
    client_id: Uuid,
    redirect_uri: &'a str,
    scopes: &'a [String],
    state: Option<&'a str>,
    code_challenge: &'a str,
    resource: &'a str,
    expires_at: DateTime<Utc>,
}

/// `GET /oauth/authorize`.
pub(crate) async fn authorize(
    State(server): State<Server>,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let mut connection = match server.pool().get().await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "authorization could not reach the database: {{error.message}}",
            );
            return consent_error("server_error");
        }
    };

    // Before the redirect URI is trusted: failures stay on this server.
    let Some(client_id) = query.client_id.as_deref() else {
        return consent_error("invalid_request");
    };
    let client = match client::resolve(&server, &mut connection, client_id).await {
        Ok(client) => client,
        Err(ClientFailure::Unknown) => return consent_error("invalid_client"),
        Err(ClientFailure::Internal) => return consent_error("server_error"),
    };
    let Some(redirect_uri) = query
        .redirect_uri
        .as_deref()
        .filter(|uri| uri.len() <= redirect::MAX_REDIRECT_URI_LENGTH)
    else {
        return consent_error("invalid_request");
    };
    if !client.accepts_redirect(redirect_uri) {
        return consent_error("invalid_redirect_uri");
    }

    // After it is trusted: failures go back to the client.
    let back = Callback {
        redirect_uri,
        state: query.state.as_deref(),
        issuer: server.issuer(),
    };
    if query.response_type.as_deref() != Some("code") {
        return back.error("unsupported_response_type");
    }
    let Some(code_challenge) = query
        .code_challenge
        .as_deref()
        .filter(|challenge| pkce::is_challenge(challenge))
    else {
        return back.error("invalid_request");
    };
    if query.code_challenge_method.as_deref() != Some(pkce::METHOD) {
        return back.error("invalid_request");
    }
    if query
        .state
        .as_deref()
        .is_some_and(|state| state.len() > MAX_STATE_LENGTH)
    {
        return back.error("invalid_request");
    }
    let Ok(scopes) = server.scopes().parse_request(query.scope.as_deref()) else {
        return back.error("invalid_scope");
    };
    if query
        .resource
        .as_deref()
        .is_some_and(|resource| !server.is_resource(resource))
    {
        return back.error("invalid_target");
    }

    let stored = store_request(
        &mut connection,
        &NewAuthorizationRequest {
            client_id: client.id,
            redirect_uri,
            scopes: &scopes,
            state: query.state.as_deref(),
            code_challenge,
            resource: server.resource(),
            expires_at: Utc::now() + Duration::minutes(REQUEST_TTL_MINUTES),
        },
    )
    .await;
    match stored {
        Ok(request_id) => {
            Redirect::to(&format!("{CONSENT_PATH}?request={request_id}")).into_response()
        }
        Err(error) => {
            tracing::error!(
                error.message = %error,
                "an authorization request could not be stored: {{error.message}}",
            );
            back.error("server_error")
        }
    }
}

/// Stores a validated request, sweeping the ones nobody decided in time.
async fn store_request(
    connection: &mut AsyncPgConnection,
    request: &NewAuthorizationRequest<'_>,
) -> QueryResult<Uuid> {
    diesel::delete(
        oauth_authorization_requests::table
            .filter(oauth_authorization_requests::expires_at.le(Utc::now())),
    )
    .execute(connection)
    .await?;

    diesel::insert_into(oauth_authorization_requests::table)
        .values(request)
        .returning(oauth_authorization_requests::id)
        .get_result(connection)
        .await
}

/// Sends the browser to the consent screen's error state.
fn consent_error(code: &str) -> Response {
    Redirect::to(&format!("{CONSENT_PATH}?error={code}")).into_response()
}

/// Builds responses on a redirect URI that has been checked.
struct Callback<'a> {
    redirect_uri: &'a str,
    state: Option<&'a str>,
    issuer: &'a str,
}

impl Callback<'_> {
    /// The redirect URI with `pairs`, the client's `state`, and `iss` appended.
    fn url(&self, pairs: &[(&str, &str)]) -> String {
        // The URI was matched against a registration that parsed, so it
        // parses; a URI that somehow does not is answered without the
        // parameters rather than with a panic.
        let Ok(mut url) = Url::parse(self.redirect_uri) else {
            tracing::error!(
                oauth.redirect_uri = self.redirect_uri,
                "a matched redirect URI did not parse: {{oauth.redirect_uri}}",
            );
            return self.redirect_uri.to_owned();
        };
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in pairs {
                query.append_pair(name, value);
            }
            if let Some(state) = self.state {
                query.append_pair("state", state);
            }
            query.append_pair("iss", self.issuer);
        };
        url.into()
    }

    fn error(&self, code: &str) -> Response {
        Redirect::to(&self.url(&[("error", code)])).into_response()
    }
}

/// The client as the consent screen introduces it.
#[derive(Debug, Serialize)]
pub(crate) struct ClientView {
    pub(crate) name: String,
    pub(crate) client_id: String,
    pub(crate) kind: ClientKind,
    /// The host that vouches for the client: the metadata document's host,
    /// which the client cannot fake, or nothing for a dynamic registration,
    /// whose name is the client's word alone.
    pub(crate) verified_host: Option<String>,
    pub(crate) client_uri: Option<String>,
}

impl ClientView {
    pub(crate) fn of(client: &ClientRow) -> Self {
        let verified_host = (client.kind() == ClientKind::MetadataDocument)
            .then(|| Url::parse(&client.client_id).ok())
            .flatten()
            .and_then(|url| url.host_str().map(str::to_owned));
        Self {
            name: client.name.clone(),
            client_id: client.client_id.clone(),
            kind: client.kind(),
            verified_host,
            client_uri: client.client_uri.clone(),
        }
    }
}

/// What the consent screen shows.
#[derive(Debug, Serialize)]
struct RequestView {
    id: Uuid,
    client: ClientView,
    /// The host the code will be delivered to, which the MCP specification
    /// requires the screen to show.
    redirect_host: String,
    /// True when the code goes to a program on this device, which the screen
    /// warns about: any program on the device could have asked.
    redirect_is_loopback: bool,
    scopes: Vec<crate::oauth_server::Scope>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = oauth_authorization_requests)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct StoredRequest {
    id: Uuid,
    redirect_uri: String,
    scopes: Vec<String>,
    state: Option<String>,
    code_challenge: String,
    resource: String,
    expires_at: DateTime<Utc>,
}

/// `GET /oauth/requests/{id}`: the request a person is about to decide.
pub(crate) async fn show(
    State(server): State<Server>,
    CurrentUser(_user): CurrentUser,
    Path(request_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "the consent screen could not reach the database: {{error.message}}",
        );
        ApiError::internal()
    })?;

    let (request, client): (StoredRequest, ClientRow) = oauth_authorization_requests::table
        .inner_join(oauth_clients::table)
        .filter(oauth_authorization_requests::id.eq(request_id))
        .filter(oauth_authorization_requests::expires_at.gt(Utc::now()))
        .select((StoredRequest::as_select(), ClientRow::as_select()))
        .first(&mut connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)?;

    let redirect = Url::parse(&request.redirect_uri).ok();
    let redirect_host = redirect
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or_default()
        .to_owned();
    let redirect_is_loopback = redirect.as_ref().is_some_and(redirect::is_loopback);
    let scopes = request
        .scopes
        .iter()
        .filter_map(|name| server.scopes().find(name).cloned())
        .collect();

    Ok(Json(RequestView {
        id: request.id,
        client: ClientView::of(&client),
        redirect_host,
        redirect_is_loopback,
        scopes,
        expires_at: request.expires_at,
    }))
}

/// The person's answer.
#[derive(Debug, Deserialize)]
pub(crate) struct DecisionBody {
    approve: bool,
}

/// Where the browser goes once the decision is recorded.
#[derive(Debug, Serialize)]
struct DecisionResponse {
    redirect_to: String,
}

/// `POST /oauth/requests/{id}`: approve or deny.
///
/// The request is consumed by the first decision, deleted with `RETURNING`, so
/// two decisions arriving together produce one code at most. Approving
/// creates the grant and its code in the same transaction as the audit event
/// that records it.
pub(crate) async fn decide(
    State(server): State<Server>,
    CurrentUser(user): CurrentUser,
    context: audit::Context,
    Path(request_id): Path<Uuid>,
    Json(body): Json<DecisionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "the consent decision could not reach the database: {{error.message}}",
        );
        ApiError::internal()
    })?;

    let redirect_to = connection
        .transaction::<_, ApiError, _>(async |transaction| {
            let consumed: Option<(Uuid, StoredRequest)> = diesel::delete(
                oauth_authorization_requests::table
                    .filter(oauth_authorization_requests::id.eq(request_id))
                    .filter(oauth_authorization_requests::expires_at.gt(Utc::now())),
            )
            .returning((
                oauth_authorization_requests::client_id,
                StoredRequest::as_returning(),
            ))
            .get_result(transaction)
            .await
            .optional()?;
            let Some((client_row_id, request)) = consumed else {
                return Err(ApiError::not_found());
            };

            let back = Callback {
                redirect_uri: &request.redirect_uri,
                state: request.state.as_deref(),
                issuer: server.issuer(),
            };
            if !body.approve {
                return Ok(back.url(&[("error", "access_denied")]));
            }

            let client: ClientRow = oauth_clients::table
                .filter(oauth_clients::id.eq(client_row_id))
                .select(ClientRow::as_select())
                .first(transaction)
                .await?;
            let granted = grant::create(
                transaction,
                user.id,
                client.id,
                &request.scopes,
                &request.resource,
            )
            .await?;

            let code = token::generate();
            diesel::insert_into(oauth_authorization_codes::table)
                .values((
                    oauth_authorization_codes::grant_id.eq(granted.id),
                    oauth_authorization_codes::code_hash.eq(token::hash(&code)),
                    oauth_authorization_codes::redirect_uri.eq(&request.redirect_uri),
                    oauth_authorization_codes::code_challenge.eq(&request.code_challenge),
                    oauth_authorization_codes::expires_at
                        .eq(Utc::now() + Duration::minutes(CODE_TTL_MINUTES)),
                ))
                .execute(transaction)
                .await?;

            grant::record(
                transaction,
                &context,
                audit::OAUTH_GRANTED,
                &granted,
                &client.name,
            )
            .await?;

            Ok(back.url(&[("code", code.as_str())]))
        })
        .await?;

    Ok(Json(DecisionResponse { redirect_to }))
}

#[cfg(test)]
mod tests {
    use super::Callback;

    #[test]
    fn a_callback_carries_the_state_and_the_issuer() {
        let back = Callback {
            redirect_uri: "http://localhost:4000/callback?keep=1",
            state: Some("xyz & more"),
            issuer: "https://app.example.com",
        };

        assert_eq!(
            back.url(&[("code", "abc")]),
            "http://localhost:4000/callback?keep=1&code=abc&state=xyz+%26+more\
             &iss=https%3A%2F%2Fapp.example.com",
        );
    }
}
