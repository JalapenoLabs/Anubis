//! Opaque-token browser sessions stored in Postgres.
//!
//! A session is a random 256-bit token handed to the browser in an `HttpOnly`
//! cookie. The database stores only the token's SHA-256, so a leaked database
//! never yields usable session tokens, and the token itself is unforgeable
//! without guessing 256 bits. Expiry is enforced server-side on every lookup;
//! expired rows for a user are swept when that user signs in again.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::model::User;
use crate::auth::token;
use crate::schema::{sessions, users};

/// Name of the session cookie set at registration and login.
pub const SESSION_COOKIE: &str = "anubis_session";

/// How long a session lives. Fixed expiry; sliding renewal can come later.
pub const SESSION_TTL_DAYS: i64 = 30;

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
pub(crate) async fn create(
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
pub(crate) async fn find_user(
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
pub(crate) async fn delete(
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
///
/// Called after a password reset so a stolen session cannot outlive the
/// credentials it was created with.
pub(crate) async fn delete_all_for_user(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::delete(sessions::table.filter(sessions::user_id.eq(user_id)))
        .execute(connection)
        .await?;

    Ok(())
}
