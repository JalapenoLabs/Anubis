//! Sessions kept in the `sessions` table, the default store.
//!
//! One row per session, holding the token's SHA-256 and never the token.
//! Every function takes the caller's connection, so a session written or
//! revoked inside a transaction commits or rolls back with the rest of it.
//! Expired rows are filtered on every read and swept for a user when that user
//! signs in again; the `users` foreign key cascades, so a deleted account takes
//! its rows with it.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::{ListedSession, SESSION_TTL_DAYS};
use crate::auth::model::User;
use crate::auth::token;
use crate::schema::{sessions, users};

#[derive(Insertable)]
#[diesel(table_name = sessions)]
struct NewSession<'a> {
    user_id: Uuid,
    token_hash: &'a str,
    expires_at: DateTime<Utc>,
}

/// Creates a session for `user_id` and returns the raw token for the cookie.
///
/// Also sweeps the user's expired sessions, so stale rows cannot accumulate
/// past one sign-in.
pub(super) async fn create(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<String, diesel::result::Error> {
    diesel::delete(
        sessions::table
            .filter(sessions::user_id.eq(user_id))
            .filter(sessions::expires_at.le(Utc::now())),
    )
    .execute(connection)
    .await?;

    let token = token::generate();
    let token_hash = token::hash(&token);

    diesel::insert_into(sessions::table)
        .values(NewSession {
            user_id,
            token_hash: &token_hash,
            expires_at: Utc::now() + Duration::days(SESSION_TTL_DAYS),
        })
        .execute(connection)
        .await?;

    Ok(token)
}

/// Resolves a raw session token to its user, if the session is live.
pub(super) async fn find_user(
    connection: &mut AsyncPgConnection,
    token: &str,
) -> Result<Option<User>, diesel::result::Error> {
    let token_hash = token::hash(token);

    sessions::table
        .inner_join(users::table)
        .filter(sessions::token_hash.eq(&token_hash))
        .filter(sessions::expires_at.gt(Utc::now()))
        .select(User::as_select())
        .first(connection)
        .await
        .optional()
}

/// Deletes the session behind a raw token, signing that browser out.
pub(super) async fn revoke(
    connection: &mut AsyncPgConnection,
    token: &str,
) -> Result<(), diesel::result::Error> {
    let token_hash = token::hash(token);

    diesel::delete(sessions::table.filter(sessions::token_hash.eq(&token_hash)))
        .execute(connection)
        .await?;

    Ok(())
}

/// Deletes every session a user has, signing out all of their browsers.
pub(super) async fn revoke_all(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::delete(sessions::table.filter(sessions::user_id.eq(user_id)))
        .execute(connection)
        .await?;

    Ok(())
}

/// Deletes every session a user has except the one behind `keep_token`.
pub(super) async fn revoke_all_except(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    keep_token: &str,
) -> Result<(), diesel::result::Error> {
    let keep_hash = token::hash(keep_token);

    diesel::delete(
        sessions::table
            .filter(sessions::user_id.eq(user_id))
            .filter(sessions::token_hash.ne(&keep_hash)),
    )
    .execute(connection)
    .await?;

    Ok(())
}

/// Lists a user's live sessions, oldest first, marking the one behind
/// `current_token`.
pub(super) async fn list(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    current_token: Option<&str>,
) -> Result<Vec<ListedSession>, diesel::result::Error> {
    let current_hash = current_token.map(token::hash);

    let rows: Vec<(Uuid, DateTime<Utc>, DateTime<Utc>, String)> = sessions::table
        .filter(sessions::user_id.eq(user_id))
        .filter(sessions::expires_at.gt(Utc::now()))
        .select((
            sessions::id,
            sessions::created_at,
            sessions::expires_at,
            sessions::token_hash,
        ))
        .order(sessions::created_at.asc())
        .load(connection)
        .await?;

    let listed = rows
        .into_iter()
        .map(|(id, created_at, expires_at, token_hash)| ListedSession {
            id,
            created_at,
            expires_at,
            current: current_hash.as_deref() == Some(token_hash.as_str()),
        })
        .collect();
    Ok(listed)
}

/// Deletes the user's session with `session_id`, answering whether one went.
///
/// Scoped to the user, so another account's session id deletes nothing.
pub(super) async fn revoke_by_id(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let deleted = diesel::delete(
        sessions::table
            .filter(sessions::id.eq(session_id))
            .filter(sessions::user_id.eq(user_id)),
    )
    .execute(connection)
    .await?;

    Ok(deleted > 0)
}
