//! Who is asking: clients from a metadata document, or registered dynamically.
//!
//! The MCP specification orders a client's choices: a Client ID Metadata
//! Document when the server advertises support, Dynamic Client Registration
//! (RFC 7591) otherwise. Claude and Codex both take the document when this
//! server's metadata says `client_id_metadata_document_supported` and lists
//! `none` among the token endpoint's authentication methods, which it does, so
//! registration exists for the clients that predate documents.
//!
//! Both kinds land in `oauth_clients`, and the row records which kind it is.
//!
//! # Registration on a deployment that assumes it will be attacked
//!
//! `POST /oauth/register` is open to anybody, so it is bounded three ways:
//!
//! - **Per client address**: [`Budget::ClientRegistration`], ten an hour.
//! - **In what it accepts**: a public client only (no secret is ever issued),
//!   the authorization code and refresh token grants only, 1 to 10 redirect
//!   URIs each passing [`redirect::check_registrable`], and a name of at most
//!   100 characters.
//! - **In what it keeps**: a registration nobody completed a grant with is
//!   swept a day later, and while [`MAX_UNUSED_REGISTRATIONS`] such rows exist
//!   the endpoint refuses with `503`. Rotating addresses therefore fills a
//!   bounded table rather than an unbounded one.
//!
//! [`Budget::ClientRegistration`]: crate::rate_limit::Budget::ClientRegistration

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::token;
use crate::oauth_server::Server;
use crate::oauth_server::document::{MAX_CLIENT_NAME_LENGTH, MAX_REDIRECT_URIS};
use crate::oauth_server::error::OauthError;
use crate::oauth_server::redirect;
use crate::schema::{oauth_clients, oauth_grants};

/// How long a dynamic registration may wait for its first grant.
///
/// A client registers immediately before it sends the person to consent, so a
/// registration a day old with no grant behind it was abandoned.
const UNUSED_REGISTRATION_TTL_HOURS: i64 = 24;

/// How many unused registrations may exist at once before the endpoint refuses.
///
/// Far above what real clients produce (one per person connecting, used within
/// minutes), and low enough that a flood from rotating addresses costs a
/// bounded number of small rows.
pub(crate) const MAX_UNUSED_REGISTRATIONS: i64 = 5_000;

/// The name a dynamic client gets when it gives none.
const UNNAMED_CLIENT: &str = "Unnamed application";

/// How a client came to be known, stored rather than inferred from its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientKind {
    /// Identified by the https URL of a document this server fetched.
    MetadataDocument,
    /// Registered through `POST /oauth/register`.
    Dynamic,
}

impl ClientKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MetadataDocument => "metadata_document",
            Self::Dynamic => "dynamic",
        }
    }

    fn parse(value: &str) -> Self {
        if value == Self::MetadataDocument.as_str() {
            return Self::MetadataDocument;
        }
        Self::Dynamic
    }
}

/// One known client.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = oauth_clients)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct ClientRow {
    pub(crate) id: Uuid,
    pub(crate) client_id: String,
    kind: String,
    pub(crate) name: String,
    pub(crate) client_uri: Option<String>,
    pub(crate) redirect_uris: Vec<String>,
    refresh_after: Option<DateTime<Utc>>,
}

impl ClientRow {
    pub(crate) fn kind(&self) -> ClientKind {
        ClientKind::parse(&self.kind)
    }

    /// The registered redirect URI `presented` matches, if any.
    pub(crate) fn accepts_redirect(&self, presented: &str) -> bool {
        self.redirect_uris
            .iter()
            .any(|registered| redirect::matches(registered, presented))
    }
}

/// Why a client id resolved to nothing usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientFailure {
    /// No client by that id, or a document that failed its checks.
    Unknown,
    /// The database failed; the cause is already logged.
    Internal,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = oauth_clients)]
struct NewClient<'a> {
    client_id: &'a str,
    kind: &'a str,
    name: &'a str,
    client_uri: Option<&'a str>,
    redirect_uris: &'a [String],
    refresh_after: Option<DateTime<Utc>>,
}

/// Resolves `client_id` to a client, fetching its document when that is due.
///
/// A cached document is used until its `refresh_after`; after that it is
/// fetched again and the row is rewritten, so a client that changed its
/// redirect URIs is believed. A fetch that fails is never cached, which the
/// draft forbids, and the authorization request is refused rather than served
/// from the stale row.
pub(crate) async fn resolve(
    server: &Server,
    connection: &mut AsyncPgConnection,
    client_id: &str,
) -> Result<ClientRow, ClientFailure> {
    let cached: Option<ClientRow> = oauth_clients::table
        .filter(oauth_clients::client_id.eq(client_id))
        .select(ClientRow::as_select())
        .first(connection)
        .await
        .optional()
        .map_err(log_internal)?;

    let fetcher = server.documents();
    if !fetcher.is_document_url(client_id) {
        return cached
            .filter(|client| client.kind() == ClientKind::Dynamic)
            .ok_or(ClientFailure::Unknown);
    }

    if let Some(client) =
        cached.filter(|client| client.refresh_after.is_some_and(|due| due > Utc::now()))
    {
        return Ok(client);
    }

    let document = fetcher.fetch(client_id).await.map_err(|reason| {
        tracing::warn!(
            oauth.client_id = client_id,
            oauth.reason = reason,
            "a client metadata document was refused: {{oauth.reason}}",
        );
        ClientFailure::Unknown
    })?;

    let row = NewClient {
        client_id,
        kind: ClientKind::MetadataDocument.as_str(),
        name: &document.name,
        client_uri: document.client_uri.as_deref(),
        redirect_uris: &document.redirect_uris,
        refresh_after: Some(document.refresh_after),
    };
    diesel::insert_into(oauth_clients::table)
        .values(&row)
        .on_conflict(oauth_clients::client_id)
        .do_update()
        .set(&row)
        .returning(ClientRow::as_returning())
        .get_result(connection)
        .await
        .map_err(log_internal)
}

/// What RFC 7591 lets a client say about itself, of which this server reads
/// the fields it acts on.
#[derive(Debug, Deserialize)]
pub(crate) struct RegistrationRequest {
    #[serde(default)]
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    client_uri: Option<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}

/// The registration response, RFC 7591 section 3.2.1.
#[derive(Debug, Serialize)]
struct RegistrationResponse {
    client_id: String,
    client_id_issued_at: i64,
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: [&'static str; 2],
    response_types: [&'static str; 1],
    token_endpoint_auth_method: &'static str,
}

/// `POST /oauth/register`: Dynamic Client Registration for public clients.
pub(crate) async fn register(
    State(server): State<Server>,
    body: Result<Json<RegistrationRequest>, JsonRejection>,
) -> Result<impl IntoResponse, OauthError> {
    let Json(request) = body.map_err(|rejection| {
        OauthError::invalid_client_metadata(format!(
            "The registration must be a JSON object: {rejection}"
        ))
    })?;
    let name = check_registration(&request)?;

    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "client registration could not reach the database: {{error.message}}",
        );
        OauthError::server_error()
    })?;

    sweep_unused_registrations(&mut connection).await?;
    let unused = count_unused_registrations(&mut connection).await?;
    if unused >= MAX_UNUSED_REGISTRATIONS {
        tracing::warn!(
            oauth.unused_registrations = unused,
            "client registration refused: {{oauth.unused_registrations}} registrations are unused",
        );
        return Err(OauthError::temporarily_unavailable(
            "Registration is paused. Try again later.",
        ));
    }

    let client_id = token::generate();
    let client_uri = request
        .client_uri
        .filter(|uri| uri.starts_with("https://") && url::Url::parse(uri).is_ok());
    let created: ClientRow = diesel::insert_into(oauth_clients::table)
        .values(NewClient {
            client_id: &client_id,
            kind: ClientKind::Dynamic.as_str(),
            name: &name,
            client_uri: client_uri.as_deref(),
            redirect_uris: &request.redirect_uris,
            refresh_after: None,
        })
        .returning(ClientRow::as_returning())
        .get_result(&mut connection)
        .await?;

    Ok((
        StatusCode::CREATED,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(RegistrationResponse {
            client_id: created.client_id,
            client_id_issued_at: Utc::now().timestamp(),
            client_name: created.name,
            redirect_uris: created.redirect_uris,
            grant_types: ["authorization_code", "refresh_token"],
            response_types: ["code"],
            token_endpoint_auth_method: "none",
        }),
    ))
}

/// Holds a registration to what this server supports, answering the name to
/// store.
fn check_registration(request: &RegistrationRequest) -> Result<String, OauthError> {
    if request
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
    {
        return Err(OauthError::invalid_client_metadata(
            "Only public clients register here: token_endpoint_auth_method must be none.",
        ));
    }
    let unsupported_grant = request
        .grant_types
        .iter()
        .flatten()
        .any(|grant| grant != "authorization_code" && grant != "refresh_token");
    if unsupported_grant {
        return Err(OauthError::invalid_client_metadata(
            "Only the authorization_code and refresh_token grants are supported.",
        ));
    }
    if request
        .response_types
        .iter()
        .flatten()
        .any(|response_type| response_type != "code")
    {
        return Err(OauthError::invalid_client_metadata(
            "Only the code response type is supported.",
        ));
    }

    if request.redirect_uris.is_empty() || request.redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(OauthError::invalid_redirect_uri(
            "Register 1 to 10 redirect URIs.",
        ));
    }
    for uri in &request.redirect_uris {
        redirect::check_registrable(uri).map_err(OauthError::invalid_redirect_uri)?;
    }

    let name = request
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(UNNAMED_CLIENT);
    if name.chars().count() > MAX_CLIENT_NAME_LENGTH {
        return Err(OauthError::invalid_client_metadata(
            "client_name must be at most 100 characters.",
        ));
    }
    Ok(name.to_owned())
}

/// Deletes dynamic registrations that never led to a grant within a day.
async fn sweep_unused_registrations(connection: &mut AsyncPgConnection) -> Result<(), OauthError> {
    let cutoff = Utc::now() - Duration::hours(UNUSED_REGISTRATION_TTL_HOURS);
    diesel::delete(
        oauth_clients::table
            .filter(oauth_clients::kind.eq(ClientKind::Dynamic.as_str()))
            .filter(oauth_clients::created_at.lt(cutoff))
            .filter(diesel::dsl::not(diesel::dsl::exists(
                oauth_grants::table.filter(oauth_grants::client_id.eq(oauth_clients::id)),
            ))),
    )
    .execute(connection)
    .await?;
    Ok(())
}

/// How many dynamic registrations have no grant behind them.
async fn count_unused_registrations(connection: &mut AsyncPgConnection) -> Result<i64, OauthError> {
    let count = oauth_clients::table
        .filter(oauth_clients::kind.eq(ClientKind::Dynamic.as_str()))
        .filter(diesel::dsl::not(diesel::dsl::exists(
            oauth_grants::table.filter(oauth_grants::client_id.eq(oauth_clients::id)),
        )))
        .count()
        .get_result(connection)
        .await?;
    Ok(count)
}

fn log_internal(error: impl std::fmt::Display) -> ClientFailure {
    tracing::error!(
        error.message = %error,
        "client lookup failed: {{error.message}}",
    );
    ClientFailure::Internal
}

#[cfg(test)]
mod tests {
    use super::{RegistrationRequest, check_registration};

    fn request(value: serde_json::Value) -> RegistrationRequest {
        serde_json::from_value(value).expect("the test registration parses")
    }

    #[test]
    fn a_native_public_client_registers() {
        let name = check_registration(&request(serde_json::json!({
            "client_name": "Claude Code",
            "redirect_uris": ["http://localhost/callback"],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })))
        .expect("a public native client is accepted");

        assert_eq!(name, "Claude Code");
    }

    #[test]
    fn a_client_without_a_name_is_named_for_it() {
        let name = check_registration(&request(serde_json::json!({
            "redirect_uris": ["https://example.com/callback"],
        })))
        .expect("a name is optional");

        assert_eq!(name, "Unnamed application");
    }

    #[test]
    fn a_confidential_client_or_other_grant_is_refused() {
        for refused in [
            serde_json::json!({
                "redirect_uris": ["https://example.com/callback"],
                "token_endpoint_auth_method": "client_secret_basic",
            }),
            serde_json::json!({
                "redirect_uris": ["https://example.com/callback"],
                "grant_types": ["client_credentials"],
            }),
            serde_json::json!({
                "redirect_uris": ["https://example.com/callback"],
                "response_types": ["token"],
            }),
            serde_json::json!({ "redirect_uris": [] }),
            serde_json::json!({ "redirect_uris": ["http://example.com/callback"] }),
        ] {
            check_registration(&request(refused.clone())).expect_err(&refused.to_string());
        }
    }
}
