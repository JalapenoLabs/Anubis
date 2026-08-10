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
    /// A pending second factor between password success and session issuance.
    MfaChallenge,
    /// A passwordless sign-in code delivered by email.
    EmailSignIn,
}

impl TokenPurpose {
    /// The value stored in the `purpose` column.
    fn as_str(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailChange => "email_change",
            Self::MfaChallenge => "mfa_challenge",
            Self::EmailSignIn => "email_sign_in",
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
            Self::MfaChallenge => Duration::minutes(5),
            Self::EmailSignIn => Duration::minutes(10),
        }
    }
}

/// Failed attempts allowed against one attempt-limited token.
pub(crate) const MAX_TOKEN_ATTEMPTS: i32 = 5;

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

/// A live token row observed without consuming it.
pub(crate) struct PeekedToken {
    pub id: Uuid,
    pub user_id: Uuid,
    #[expect(dead_code, reason = "read by the passkey flow, landing next")]
    pub payload: Option<String>,
}

/// Looks a token up without consuming it, for attempt-limited flows.
///
/// The caller verifies a guessable secret against the peeked row, then either
/// deletes the token on success ([`delete_by_id`]) or records the failure
/// ([`record_failure`]), which destroys the token once attempts run out.
pub(crate) async fn peek(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
    purpose: TokenPurpose,
) -> Result<Option<PeekedToken>, diesel::result::Error> {
    let token_hash = token::hash(raw_token);

    let row: Option<(Uuid, Uuid, Option<String>)> = user_tokens::table
        .filter(user_tokens::token_hash.eq(&token_hash))
        .filter(user_tokens::purpose.eq(purpose.as_str()))
        .filter(user_tokens::expires_at.gt(Utc::now()))
        .filter(user_tokens::attempts.lt(MAX_TOKEN_ATTEMPTS))
        .select((user_tokens::id, user_tokens::user_id, user_tokens::payload))
        .first(connection)
        .await
        .optional()?;

    Ok(row.map(|(id, user_id, payload)| PeekedToken {
        id,
        user_id,
        payload,
    }))
}

/// Records a failed attempt; the token dies when attempts run out.
pub(crate) async fn record_failure(
    connection: &mut AsyncPgConnection,
    token_id: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::update(user_tokens::table.find(token_id))
        .set(user_tokens::attempts.eq(user_tokens::attempts + 1))
        .execute(connection)
        .await?;

    diesel::delete(
        user_tokens::table
            .find(token_id)
            .filter(user_tokens::attempts.ge(MAX_TOKEN_ATTEMPTS)),
    )
    .execute(connection)
    .await?;

    Ok(())
}

/// Deletes a token by id after its flow succeeds.
pub(crate) async fn delete_by_id(
    connection: &mut AsyncPgConnection,
    token_id: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::delete(user_tokens::table.find(token_id))
        .execute(connection)
        .await?;

    Ok(())
}

/// Issues a 6-digit code for a user, replacing any outstanding one.
///
/// Short codes are guessable, so they are looked up by user (not by code),
/// attempt-limited, and short-lived per the purpose's lifetime.
pub(crate) async fn issue_short_code(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    purpose: TokenPurpose,
) -> Result<String, diesel::result::Error> {
    diesel::delete(
        user_tokens::table
            .filter(user_tokens::user_id.eq(user_id))
            .filter(user_tokens::purpose.eq(purpose.as_str())),
    )
    .execute(connection)
    .await?;

    let code = generate_short_code();
    let token_hash = token::hash(&code);

    diesel::insert_into(user_tokens::table)
        .values(NewUserToken {
            user_id,
            purpose: purpose.as_str(),
            token_hash: &token_hash,
            expires_at: Utc::now() + purpose.ttl(),
            payload: None,
        })
        .execute(connection)
        .await?;

    Ok(code)
}

/// Verifies a user's short code: deletes it on success, counts failures.
///
/// Returns `true` only for a live, attempt-eligible, matching code.
pub(crate) async fn verify_short_code(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    purpose: TokenPurpose,
    submitted: &str,
) -> Result<bool, diesel::result::Error> {
    let row: Option<(Uuid, String)> = user_tokens::table
        .filter(user_tokens::user_id.eq(user_id))
        .filter(user_tokens::purpose.eq(purpose.as_str()))
        .filter(user_tokens::expires_at.gt(Utc::now()))
        .filter(user_tokens::attempts.lt(MAX_TOKEN_ATTEMPTS))
        .select((user_tokens::id, user_tokens::token_hash))
        .first(connection)
        .await
        .optional()?;

    let Some((token_id, stored_hash)) = row else {
        return Ok(false);
    };

    if token::hash(submitted.trim()) == stored_hash {
        delete_by_id(connection, token_id).await?;
        Ok(true)
    } else {
        record_failure(connection, token_id).await?;
        Ok(false)
    }
}

/// A uniformly random 6-digit code, rejection-sampled to avoid modulo bias.
fn generate_short_code() -> String {
    // Largest multiple of 1_000_000 that fits in u32; values above it would
    // bias the low buckets.
    const REJECTION_THRESHOLD: u32 = 4_294_000_000;

    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).expect("the OS random source must be available");
        let value = u32::from_be_bytes(bytes);
        if value < REJECTION_THRESHOLD {
            return format!("{:06}", value % 1_000_000);
        }
    }
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
