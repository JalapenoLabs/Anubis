//! Single-use tokens for email verification and password reset.
//!
//! Same discipline as sessions: the client gets a random 256-bit token, the
//! database stores only its SHA-256. Tokens are single-use (consumption
//! deletes the row atomically) and short-lived, with the lifetime chosen per
//! purpose. Issuing a new token replaces any outstanding one for the same
//! user and purpose.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::token;
use crate::schema::user_tokens;

/// What a single-use token authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenPurpose {
    /// Confirms the user controls their email address.
    EmailVerification,
    /// Authorizes setting a new password without knowing the old one.
    PasswordReset,
    /// Authorizes swapping to the new email carried in the payload.
    EmailChange,
}

impl TokenPurpose {
    /// The value stored in the `purpose` column.
    fn as_str(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailChange => "email_change",
        }
    }

    /// How long a token of this purpose stays valid.
    ///
    /// Verification links sit in inboxes, so they get days; reset and change
    /// links are requested and used immediately, so they get minutes.
    fn ttl(self) -> Duration {
        match self {
            Self::EmailVerification => Duration::days(3),
            Self::PasswordReset => Duration::minutes(30),
            Self::EmailChange => Duration::hours(1),
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = user_tokens)]
struct NewUserToken<'a> {
    user_id: Uuid,
    purpose: &'a str,
    token_hash: &'a str,
    expires_at: DateTime<Utc>,
    payload: Option<&'a str>,
}

/// Issues a token for `user_id`, replacing any outstanding one.
///
/// Returns the raw token for embedding in an email link.
pub(crate) async fn issue(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    purpose: TokenPurpose,
) -> Result<String, diesel::result::Error> {
    issue_with_payload(connection, user_id, purpose, None).await
}

/// Issues a token carrying flow-specific data, replacing any outstanding one.
pub(crate) async fn issue_with_payload(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    purpose: TokenPurpose,
    payload: Option<&str>,
) -> Result<String, diesel::result::Error> {
    diesel::delete(
        user_tokens::table
            .filter(user_tokens::user_id.eq(user_id))
            .filter(user_tokens::purpose.eq(purpose.as_str())),
    )
    .execute(connection)
    .await?;

    let raw_token = token::generate();
    let token_hash = token::hash(&raw_token);

    diesel::insert_into(user_tokens::table)
        .values(NewUserToken {
            user_id,
            purpose: purpose.as_str(),
            token_hash: &token_hash,
            expires_at: Utc::now() + purpose.ttl(),
            payload,
        })
        .execute(connection)
        .await?;

    Ok(raw_token)
}

/// Consumes a raw token, returning the user it belonged to and its payload.
///
/// Consumption deletes the row in the same statement that matches it, so a
/// token can never be used twice. Returns `None` for unknown, expired, or
/// wrong-purpose tokens.
pub(crate) async fn consume(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
    purpose: TokenPurpose,
) -> Result<Option<(Uuid, Option<String>)>, diesel::result::Error> {
    let token_hash = token::hash(raw_token);

    diesel::delete(
        user_tokens::table
            .filter(user_tokens::token_hash.eq(&token_hash))
            .filter(user_tokens::purpose.eq(purpose.as_str()))
            .filter(user_tokens::expires_at.gt(Utc::now())),
    )
    .returning((user_tokens::user_id, user_tokens::payload))
    .get_result(connection)
    .await
    .optional()
}
