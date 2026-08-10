//! Opaque-token browser sessions stored in Postgres.
//!
//! A session is a random 256-bit token handed to the browser in an `HttpOnly`
//! cookie. The database stores only the token's SHA-256, so a leaked database
//! never yields usable session tokens, and the token itself is unforgeable
//! without guessing 256 bits. Expiry is enforced server-side on every lookup;
//! expired rows for a user are swept when that user signs in again.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::model::User;
use crate::schema::{sessions, users};

/// Name of the session cookie set at registration and login.
pub const SESSION_COOKIE: &str = "anubis_session";

/// How long a session lives. Fixed expiry; sliding renewal can come later.
pub const SESSION_TTL_DAYS: i64 = 30;

/// Random bytes per token; 32 bytes = 256 bits of entropy.
const TOKEN_BYTES: usize = 32;

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

    let token = generate_token();
    let token_hash = hash_token(&token);

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
    let token_hash = hash_token(token);

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
    let token_hash = hash_token(token);

    diesel::delete(sessions::table.filter(sessions::token_hash.eq(&token_hash)))
        .execute(connection)
        .await?;

    Ok(())
}

fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random source must be available");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::{generate_token, hash_token};

    #[test]
    fn tokens_are_long_random_and_unique() {
        let first = generate_token();
        let second = generate_token();

        assert_ne!(first, second);
        // 32 bytes of base64url without padding is 43 characters.
        assert_eq!(first.len(), 43);
    }

    #[test]
    fn hashes_are_stable_and_do_not_reveal_the_token() {
        let token = generate_token();

        assert_eq!(hash_token(&token), hash_token(&token));
        assert_ne!(hash_token(&token), token);
        assert!(!hash_token(&token).contains(&token));
    }
}
