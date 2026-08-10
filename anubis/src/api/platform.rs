//! Platform applications: the per-team credentials behind the public API.
//!
//! A platform application is a named credential a team creates in the
//! "Developers" section. Creating one provisions a non-expiring bearer token
//! (shown exactly once); rotation replaces every token the application has.
//! Tokens follow the framework discipline: random 256 bits to the caller,
//! SHA-256 at rest.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::token;
use crate::schema::{platform_applications, platform_tokens, teams};
use crate::tenancy::Team;

/// A named API credential belonging to a team.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = platform_applications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PlatformApplication {
    /// Primary key.
    pub id: Uuid,
    /// The team the application belongs to; its tokens act as this team.
    pub team_id: Uuid,
    /// Display name, e.g. "Zapier integration".
    pub name: String,
    /// When the application was created.
    pub created_at: DateTime<Utc>,
    /// When the application was last updated.
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = platform_applications)]
struct NewPlatformApplication<'a> {
    team_id: Uuid,
    name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = platform_tokens)]
struct NewPlatformToken<'a> {
    platform_application_id: Uuid,
    token_hash: &'a str,
}

/// Creates an application and provisions its first bearer token.
///
/// Returns the application and the raw token; the token is not recoverable
/// afterwards.
pub(crate) async fn create(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    name: &str,
) -> Result<(PlatformApplication, String), diesel::result::Error> {
    let application: PlatformApplication = diesel::insert_into(platform_applications::table)
        .values(NewPlatformApplication { team_id, name })
        .returning(PlatformApplication::as_returning())
        .get_result(connection)
        .await?;

    let raw_token = issue_token(connection, application.id).await?;
    Ok((application, raw_token))
}

/// Replaces every token the application has with one fresh token.
pub(crate) async fn rotate_token(
    connection: &mut AsyncPgConnection,
    application_id: Uuid,
) -> Result<String, diesel::result::Error> {
    diesel::delete(
        platform_tokens::table.filter(platform_tokens::platform_application_id.eq(application_id)),
    )
    .execute(connection)
    .await?;

    issue_token(connection, application_id).await
}

async fn issue_token(
    connection: &mut AsyncPgConnection,
    application_id: Uuid,
) -> Result<String, diesel::result::Error> {
    let raw_token = token::generate();
    let token_hash = token::hash(&raw_token);

    diesel::insert_into(platform_tokens::table)
        .values(NewPlatformToken {
            platform_application_id: application_id,
            token_hash: &token_hash,
        })
        .execute(connection)
        .await?;

    Ok(raw_token)
}

/// Resolves a raw bearer token to its application and team, if live.
pub(crate) async fn resolve_bearer(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
) -> Result<Option<(PlatformApplication, Team)>, diesel::result::Error> {
    let token_hash = token::hash(raw_token);

    platform_tokens::table
        .inner_join(platform_applications::table.inner_join(teams::table))
        .filter(platform_tokens::token_hash.eq(&token_hash))
        .filter(
            platform_tokens::expires_at
                .is_null()
                .or(platform_tokens::expires_at.gt(Some(Utc::now()))),
        )
        .select((PlatformApplication::as_select(), Team::as_select()))
        .first(connection)
        .await
        .optional()
}
