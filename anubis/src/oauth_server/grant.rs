//! Grants, the tokens that descend from them, and ending both.
//!
//! A grant is one person's consent for one client, and it is the unit
//! everything else hangs off: the authorization code that starts it, every
//! access token, and the refresh-token family. Revoking a grant is therefore
//! the one way a connection ends, whoever ends it: the person from their
//! account screen, the client through `POST /oauth/revoke`, or this server on
//! finding a spent code or a rotated refresh token presented a second time.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::auth::User;
use crate::auth::token;
use crate::schema::{
    oauth_access_tokens, oauth_authorization_codes, oauth_clients, oauth_grants,
    oauth_refresh_tokens, users,
};

/// How long an access token is honored.
///
/// One hour: long enough that an agent working through a task refreshes a
/// handful of times, short enough that a token copied out of a log is useless
/// by the time anybody reads the log. Revocation is immediate anyway, because
/// every request looks its token up.
pub(crate) const ACCESS_TOKEN_TTL: Duration = Duration::hours(1);

/// How long a refresh token waits to be used before it lapses.
///
/// Each refresh issues a new one with a new thirty days, so a connection in
/// regular use never expires and one left alone for a month does.
const REFRESH_TOKEN_TTL: Duration = Duration::days(30);

/// One grant row.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = oauth_grants)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct Grant {
    pub(crate) id: Uuid,
    pub(crate) user_id: Uuid,
    pub(crate) scopes: Vec<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) last_used_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = oauth_grants)]
struct NewGrant<'a> {
    user_id: Uuid,
    client_id: Uuid,
    scopes: &'a [String],
    resource: &'a str,
}

/// Records a person's consent.
pub(crate) async fn create(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    client_id: Uuid,
    scopes: &[String],
    resource: &str,
) -> QueryResult<Grant> {
    diesel::insert_into(oauth_grants::table)
        .values(NewGrant {
            user_id,
            client_id,
            scopes,
            resource,
        })
        .returning(Grant::as_returning())
        .get_result(connection)
        .await
}

/// Ends a grant: marks it revoked and deletes every token it issued.
///
/// The grant row stays, marked, so the moment it ended is a fact the account
/// screen and the audit log agree on. Answers whether this call is the one
/// that ended it, which is what keeps a revocation recorded once when two
/// arrive together.
pub(crate) async fn revoke(
    connection: &mut AsyncPgConnection,
    grant_id: Uuid,
) -> QueryResult<bool> {
    let ended = diesel::update(
        oauth_grants::table
            .filter(oauth_grants::id.eq(grant_id))
            .filter(oauth_grants::revoked_at.is_null()),
    )
    .set(oauth_grants::revoked_at.eq(Some(Utc::now())))
    .execute(connection)
    .await?;

    diesel::delete(oauth_access_tokens::table.filter(oauth_access_tokens::grant_id.eq(grant_id)))
        .execute(connection)
        .await?;
    diesel::delete(oauth_refresh_tokens::table.filter(oauth_refresh_tokens::grant_id.eq(grant_id)))
        .execute(connection)
        .await?;
    diesel::delete(
        oauth_authorization_codes::table.filter(oauth_authorization_codes::grant_id.eq(grant_id)),
    )
    .execute(connection)
    .await?;

    Ok(ended > 0)
}

/// A fresh access token and refresh token, as the token endpoint answers them.
#[derive(Debug)]
pub(crate) struct IssuedTokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) scopes: Vec<String>,
}

#[derive(Insertable)]
#[diesel(table_name = oauth_access_tokens)]
struct NewAccessToken<'a> {
    grant_id: Uuid,
    token_hash: &'a str,
    scopes: &'a [String],
    expires_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = oauth_refresh_tokens)]
struct NewRefreshToken<'a> {
    grant_id: Uuid,
    token_hash: &'a str,
    expires_at: DateTime<Utc>,
}

/// Issues an access token for `scopes` and the next refresh token of `grant`.
///
/// Also sweeps what the grant no longer needs: its expired access tokens,
/// refresh tokens past their lifetime, and codes past theirs. A spent refresh
/// token is kept until it expires, because presenting it again is the reuse
/// this server watches for.
pub(crate) async fn issue_tokens(
    connection: &mut AsyncPgConnection,
    grant: &Grant,
    scopes: Vec<String>,
) -> QueryResult<IssuedTokens> {
    let now = Utc::now();
    diesel::delete(
        oauth_access_tokens::table
            .filter(oauth_access_tokens::grant_id.eq(grant.id))
            .filter(oauth_access_tokens::expires_at.le(now)),
    )
    .execute(connection)
    .await?;
    diesel::delete(
        oauth_refresh_tokens::table
            .filter(oauth_refresh_tokens::grant_id.eq(grant.id))
            .filter(oauth_refresh_tokens::expires_at.le(now)),
    )
    .execute(connection)
    .await?;
    diesel::delete(
        oauth_authorization_codes::table
            .filter(oauth_authorization_codes::grant_id.eq(grant.id))
            .filter(oauth_authorization_codes::expires_at.le(now)),
    )
    .execute(connection)
    .await?;

    let access_token = token::generate();
    diesel::insert_into(oauth_access_tokens::table)
        .values(NewAccessToken {
            grant_id: grant.id,
            token_hash: &token::hash(&access_token),
            scopes: &scopes,
            expires_at: now + ACCESS_TOKEN_TTL,
        })
        .execute(connection)
        .await?;

    let refresh_token = token::generate();
    diesel::insert_into(oauth_refresh_tokens::table)
        .values(NewRefreshToken {
            grant_id: grant.id,
            token_hash: &token::hash(&refresh_token),
            expires_at: now + REFRESH_TOKEN_TTL,
        })
        .execute(connection)
        .await?;

    diesel::update(oauth_grants::table.filter(oauth_grants::id.eq(grant.id)))
        .set(oauth_grants::last_used_at.eq(now))
        .execute(connection)
        .await?;

    Ok(IssuedTokens {
        access_token,
        refresh_token,
        scopes,
    })
}

/// Records an act on a grant in its owner's account log.
///
/// The actor is the person the grant belongs to, because a client holding it
/// acts as them; the subject is the grant, labeled with the client's name so
/// the log reads "Claude Code" rather than an id. The scopes ride along as the
/// change set, since what a connection could do is the fact a reader of a
/// grant or a revocation wants.
pub(crate) async fn record(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    action: &str,
    grant: &Grant,
    client_name: &str,
) -> QueryResult<()> {
    let user: User = users::table
        .filter(users::id.eq(grant.user_id))
        .select(User::as_select())
        .first(connection)
        .await?;

    record_as(connection, &context.by(&user), action, grant, client_name).await
}

/// Records an act on a grant under the actor `attributed` already names.
///
/// [`record`] attributes to the grant's owner, because a client acts as them.
/// An act somebody else caused, an operator setting a temporary password, is
/// theirs, so the caller attributes it and this records it as given.
async fn record_as(
    connection: &mut AsyncPgConnection,
    attributed: &audit::Context,
    action: &str,
    grant: &Grant,
    client_name: &str,
) -> QueryResult<()> {
    audit::record(
        connection,
        attributed,
        &audit::Event::new(action, "OauthGrant")
            .subject(grant.id)
            .label(client_name)
            .changes(audit::Changes::new().field(
                "scopes",
                serde_json::Value::Null,
                json!(grant.scopes),
            )),
    )
    .await?;
    Ok(())
}

/// Ends every live grant `user_id` holds, recording each as revoked.
///
/// Called inside the transaction that replaces an account's password when the
/// old credentials may be in the wrong hands: an operator's temporary password
/// and a completed password reset. A connected client is a credential like a
/// session, so it goes with the sessions; a voluntary password change keeps
/// both, because the person proved they hold the old password. Each grant
/// records [`audit::OAUTH_REVOKED`] under `attributed`, the actor the caller
/// names, and the request id ties it to the credential event beside it.
///
/// # Errors
/// Returns the database's error; the caller's transaction rolls back with it.
pub(crate) async fn revoke_every_grant(
    connection: &mut AsyncPgConnection,
    attributed: &audit::Context,
    user_id: Uuid,
) -> QueryResult<usize> {
    let live: Vec<(Grant, String)> = oauth_grants::table
        .inner_join(oauth_clients::table)
        .filter(oauth_grants::user_id.eq(user_id))
        .filter(oauth_grants::revoked_at.is_null())
        .select((Grant::as_select(), oauth_clients::name))
        .load(connection)
        .await?;

    let mut ended = 0;
    for (revoked, client_name) in &live {
        // A concurrent revocation may have ended it first; only the call that
        // ended it records it.
        if revoke(connection, revoked.id).await? {
            record_as(
                connection,
                attributed,
                audit::OAUTH_REVOKED,
                revoked,
                client_name,
            )
            .await?;
            ended += 1;
        }
    }
    Ok(ended)
}
