//! The account's connected clients: every live grant, and revoking one.
//!
//! | Route | Effect |
//! |---|---|
//! | `GET /oauth/connections` | The signed-in account's live grants, newest first |
//! | `DELETE /oauth/connections/{grant_id}` | Revoke one, ending the connection whole |
//!
//! Both are session routes, the browser managing what the browser approved. A
//! grant that belongs to somebody else, or that already ended, answers `404`,
//! so ids reveal nothing.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::audit;
use crate::auth::CurrentUser;
use crate::http::ApiError;
use crate::oauth_server::authorize::ClientView;
use crate::oauth_server::client::ClientRow;
use crate::oauth_server::grant::{self, Grant};
use crate::oauth_server::{Scope, Server};
use crate::schema::{oauth_clients, oauth_grants};

/// One connected client, as the account screen lists it.
#[derive(Debug, Serialize)]
struct ConnectionView {
    id: Uuid,
    client: ClientView,
    scopes: Vec<Scope>,
    created_at: DateTime<Utc>,
    last_used_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct ConnectionsBody {
    connections: Vec<ConnectionView>,
}

/// `GET /oauth/connections`.
pub(crate) async fn list(
    State(server): State<Server>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = server.pool().get().await.map_err(log_unreachable)?;
    let rows: Vec<(Grant, ClientRow)> = oauth_grants::table
        .inner_join(oauth_clients::table)
        .filter(oauth_grants::user_id.eq(user.id))
        .filter(oauth_grants::revoked_at.is_null())
        .order(oauth_grants::created_at.desc())
        .select((Grant::as_select(), ClientRow::as_select()))
        .load(&mut connection)
        .await?;

    let connections = rows
        .into_iter()
        .map(|(granted, client)| ConnectionView {
            id: granted.id,
            client: ClientView::of(&client),
            // A scope the application stopped declaring is still held by the
            // grant, so it is listed by name rather than dropped from view.
            scopes: granted
                .scopes
                .iter()
                .map(|name| {
                    server
                        .scopes()
                        .find(name)
                        .cloned()
                        .unwrap_or_else(|| Scope {
                            name: name.clone(),
                            description: String::new(),
                        })
                })
                .collect(),
            created_at: granted.created_at,
            last_used_at: granted.last_used_at,
        })
        .collect();

    Ok(Json(ConnectionsBody { connections }))
}

/// `DELETE /oauth/connections/{grant_id}`.
pub(crate) async fn revoke(
    State(server): State<Server>,
    CurrentUser(user): CurrentUser,
    context: audit::Context,
    Path(grant_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = server.pool().get().await.map_err(log_unreachable)?;

    connection
        .transaction::<_, ApiError, _>(async |transaction| {
            let found: Option<(Grant, String)> = oauth_grants::table
                .inner_join(oauth_clients::table)
                .filter(oauth_grants::id.eq(grant_id))
                .filter(oauth_grants::user_id.eq(user.id))
                .filter(oauth_grants::revoked_at.is_null())
                .select((Grant::as_select(), oauth_clients::name))
                .first(transaction)
                .await
                .optional()?;
            let Some((revoked, client_name)) = found else {
                return Err(ApiError::not_found());
            };

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
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

fn log_unreachable(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "connected clients could not reach the database: {{error.message}}",
    );
    ApiError::internal()
}
