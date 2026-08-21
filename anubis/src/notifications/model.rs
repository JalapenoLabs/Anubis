//! The notification row, and the four queries the inbox is made of.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::http::ListParams;
use crate::schema::notifications;

/// One notice, addressed to one person.
///
/// The text is stored rather than recomputed: a notification says what was
/// true when it was written, and the record it talks about may since have
/// changed or been deleted. [`Notification::kind`] is what an application
/// keys its own translations off, for the same reason.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Notification {
    /// Primary key.
    pub id: Uuid,
    /// The recipient.
    pub user_id: Uuid,
    /// The team the notice is about, when it is about one.
    pub team_id: Option<Uuid>,
    /// The machine-readable type, `<subject>.<event>`.
    pub kind: String,
    /// The one line the bell shows.
    pub title: String,
    /// The detail under the title, when there is one.
    pub body: Option<String>,
    /// Where the entry navigates, as an application path.
    pub href: Option<String>,
    /// When the recipient read it; null while it is unread.
    pub read_at: Option<DateTime<Utc>>,
    /// When it was written, which is when the thing it names happened.
    pub created_at: DateTime<Utc>,
}

/// A notification about to be written.
///
/// Every field but the recipient, the kind, and the title is optional, so a
/// notice with nowhere to go and nothing more to say is three fields and two
/// `None`s. See [`notify`](super::notify) for the transactional contract.
#[derive(Debug, Insertable)]
#[diesel(table_name = notifications)]
pub struct NewNotification<'a> {
    /// Who reads it.
    pub user_id: Uuid,
    /// The team the notice is about, when it is about one.
    pub team_id: Option<Uuid>,
    /// The machine-readable type, `<subject>.<event>`.
    pub kind: &'a str,
    /// The one line the bell shows.
    pub title: &'a str,
    /// The detail under the title.
    pub body: Option<&'a str>,
    /// Where the entry navigates, as an application path.
    pub href: Option<&'a str>,
}

/// Writes one notification and returns it.
pub(super) async fn insert(
    connection: &mut AsyncPgConnection,
    notification: NewNotification<'_>,
) -> QueryResult<Notification> {
    diesel::insert_into(notifications::table)
        .values(notification)
        .returning(Notification::as_returning())
        .get_result(connection)
        .await
}

/// One page of somebody's inbox: unread first, then newest first.
///
/// Two keys rather than one, because an inbox is read for what still needs
/// attention and only then for what happened. A notification that is read
/// keeps its place in time, so marking one read moves it down the list rather
/// than out of it.
pub(super) async fn page(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    params: &ListParams,
) -> QueryResult<Vec<Notification>> {
    notifications::table
        .filter(notifications::user_id.eq(user_id))
        .order((
            notifications::read_at.is_null().desc(),
            notifications::created_at.desc(),
            // A tiebreak, so two notifications written in the same transaction
            // page deterministically instead of swapping between requests.
            notifications::id.asc(),
        ))
        .limit(params.limit())
        .offset(params.offset())
        .select(Notification::as_select())
        .load(connection)
        .await
}

/// How many notifications the recipient has, and how many are unread.
pub(super) async fn counts(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<(i64, i64)> {
    let total: i64 = notifications::table
        .filter(notifications::user_id.eq(user_id))
        .count()
        .get_result(connection)
        .await?;

    let unread: i64 = notifications::table
        .filter(notifications::user_id.eq(user_id))
        .filter(notifications::read_at.is_null())
        .count()
        .get_result(connection)
        .await?;

    Ok((total, unread))
}

/// Marks one of the recipient's own notifications read.
///
/// Returns `None` when the id is not theirs, which is also what an id that
/// does not exist returns: the two are indistinguishable on purpose, exactly
/// as the ownership guards make them.
///
/// Already-read rows keep their original `read_at`, so pressing an entry twice
/// does not move it.
pub(super) async fn mark_read(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    notification_id: Uuid,
) -> QueryResult<Option<Notification>> {
    let marked: Option<Notification> = diesel::update(
        notifications::table
            .filter(notifications::id.eq(notification_id))
            .filter(notifications::user_id.eq(user_id))
            .filter(notifications::read_at.is_null()),
    )
    .set(notifications::read_at.eq(Utc::now()))
    .returning(Notification::as_returning())
    .get_result(connection)
    .await
    .optional()?;

    if let Some(marked) = marked {
        return Ok(Some(marked));
    }

    // Nothing was updated, which means the row is already read, belongs to
    // somebody else, or does not exist. Reading it back separates the first
    // case from the other two, which still answer identically.
    notifications::table
        .filter(notifications::id.eq(notification_id))
        .filter(notifications::user_id.eq(user_id))
        .select(Notification::as_select())
        .first(connection)
        .await
        .optional()
}

/// Marks every unread notification of the recipient read, returning how many.
pub(super) async fn mark_all_read(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::update(
        notifications::table
            .filter(notifications::user_id.eq(user_id))
            .filter(notifications::read_at.is_null()),
    )
    .set(notifications::read_at.eq(Utc::now()))
    .execute(connection)
    .await
}
