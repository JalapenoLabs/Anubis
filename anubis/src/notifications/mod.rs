//! In-app notifications: a framework-owned inbox with a realtime bell.
//!
//! A notification is one row addressed to one person. The framework writes
//! them for the things it owns (invitations, roles, failing webhook
//! endpoints), an application writes its own with [`notify`], and every
//! recipient reads theirs through the session-authenticated endpoints
//! [`router`] mounts under `/account`. The whole design is written up for
//! application authors in `docs/notifications.md`.
//!
//! # Emitting
//!
//! [`notify`] is the whole producer surface, and it is [`webhooks::emit`]'s
//! sibling: the insert rides the caller's own `connection`, so a notice
//! commits with the write that caused it. A rolled-back write notifies nobody,
//! and a committed write never loses its notice.
//!
//! ```ignore
//! anubis::notifications::notify(
//!     connection,
//!     NewNotification {
//!         user_id: assignee_id,
//!         team_id: Some(team.id),
//!         kind: "project.assigned",
//!         title: &format!("{} assigned you {}", actor.email, project.name),
//!         body: None,
//!         href: Some(&format!("/projects/{}", project.id)),
//!     },
//! )
//! .await?;
//! ```
//!
//! The scaffolder deliberately emits nothing. What a model notifies about, and
//! whom, is a product decision, the same line the framework draws for billing
//! limits: `anubis scaffold model` writes the `webhooks::emit` calls, because
//! an event type is mechanical, and leaves notifications to the author.
//!
//! # Kinds
//!
//! [`Notification::kind`] is a machine-readable `<subject>.<event>` type, and
//! the framework's own are the constants below. The stored `title` and `body`
//! are English, written when the notice was: an application with a translated
//! inbox looks its copy up by `kind` and renders that instead, which is the
//! same rule that keeps enum values out of locale files.
//!
//! # Delivering
//!
//! The row is the delivery. On top of it, [`notify`] enqueues a
//! [`PingRecipient`] job so a [`Pinger`] can publish on the recipient's
//! `user:{user_id}:notifications` channel once the transaction commits; see
//! [`ping`] for why a publish cannot happen where the row is written. Nothing
//! durable rides the socket: the ping says "look again", and the browser does.
//!
//! [`webhooks::emit`]: crate::webhooks::emit

mod model;
mod ping;
mod routes;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::jobs;
use crate::schema::{organization_memberships, team_memberships};
use crate::tenancy::ADMIN_ROLE;

#[doc(inline)]
pub use model::{NewNotification, Notification};
#[doc(inline)]
pub use ping::{PingRecipient, Pinger, QUEUE, TOPIC};
#[doc(inline)]
pub use routes::router;

/// Somebody with an account was invited to a team or an organization.
///
/// Sent at invitation time, and only to an address that already has an
/// account: an invitation to a stranger is an email and nothing else, because
/// there is no inbox to write to yet.
pub const INVITATION_RECEIVED: &str = "invitation.received";

/// An invitation was claimed, told to the admins of what was joined.
pub const INVITATION_CLAIMED: &str = "invitation.claimed";

/// A member's roles were changed, told to that member.
pub const MEMBERSHIP_ROLES_CHANGED: &str = "membership.roles_changed";

/// An outgoing webhook delivery ran out of attempts, told to team admins.
pub const WEBHOOK_DELIVERY_FAILED: &str = "webhook.delivery_failed";

/// Writes one notification and asks its recipient's browsers to refetch.
///
/// The insert and the ping's enqueue both ride `connection`, which is the
/// point of keeping notifications in Postgres beside the queue: notifying
/// inside the transaction that wrote a record means a rolled-back write
/// notifies nobody, and a committed write never loses its notice.
///
/// Returns the stored row, which is what the recipient will read.
///
/// # Errors
/// Returns the underlying query error when the insert or the enqueue fails.
/// Returning a query error is deliberate: the caller is already inside a
/// Diesel transaction, so the failure joins the rollback of the write it
/// belongs to rather than needing an error type of its own.
///
/// # Examples
/// ```ignore
/// connection
///     .transaction::<_, diesel::result::Error, _>(async |connection| {
///         let project = insert_project(connection).await?;
///         anubis::notifications::notify(
///             connection,
///             anubis::notifications::NewNotification {
///                 user_id: lead_id,
///                 team_id: Some(team_id),
///                 kind: "project.assigned",
///                 title: "A project was assigned to you",
///                 body: None,
///                 href: None,
///             },
///         )
///         .await?;
///         Ok(project)
///     })
///     .await?;
/// ```
pub async fn notify(
    connection: &mut AsyncPgConnection,
    notification: NewNotification<'_>,
) -> QueryResult<Notification> {
    let user_id = notification.user_id;
    let stored = model::insert(connection, notification).await?;
    jobs::enqueue_query(connection, &PingRecipient { user_id }).await?;
    Ok(stored)
}

/// The users who administer `team_id`, the audience for a team-wide notice.
///
/// Claimed memberships only: an invited seat has nobody behind it to read an
/// inbox. Ordered by id so a caller notifying several people writes their rows
/// in a stable order.
///
/// # Errors
/// Returns the underlying query error, for the same reason [`notify`] does.
pub async fn team_admins(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    let holders: Vec<Option<Uuid>> = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::roles.contains(vec![ADMIN_ROLE.to_owned()]))
        .filter(team_memberships::user_id.is_not_null())
        .select(team_memberships::user_id)
        .order(team_memberships::user_id.asc())
        .load(connection)
        .await?;

    Ok(holders.into_iter().flatten().collect())
}

/// The users who administer `organization_id`.
///
/// The organization-level counterpart of [`team_admins`]. An organization
/// membership always names a user, so nothing is filtered out here.
///
/// # Errors
/// Returns the underlying query error, for the same reason [`notify`] does.
pub async fn organization_admins(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::roles.contains(vec![ADMIN_ROLE.to_owned()]))
        .select(organization_memberships::user_id)
        .order(organization_memberships::user_id.asc())
        .load(connection)
        .await
}

/// The path a team's settings screen is served at.
///
/// The framework stores rendered notifications, so it has to name the screen a
/// notice points at. This is a contract with the stamped application's
/// `UrlTree`, the same one `anubis::billing` relies on when it hands Stripe a
/// return URL: an application that renames the route updates both.
pub(crate) fn team_settings_href(team_id: Uuid) -> String {
    format!("/teams/{team_id}/settings")
}

/// The path a team's Developers screen is served at. See [`team_settings_href`].
pub(crate) fn team_developers_href(team_id: Uuid) -> String {
    format!("/teams/{team_id}/developers")
}

/// The path an organization's settings screen is served at.
/// See [`team_settings_href`].
pub(crate) fn organization_settings_href(organization_id: Uuid) -> String {
    format!("/organizations/{organization_id}/settings")
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        INVITATION_CLAIMED, INVITATION_RECEIVED, MEMBERSHIP_ROLES_CHANGED, WEBHOOK_DELIVERY_FAILED,
        organization_settings_href, team_developers_href, team_settings_href,
    };

    #[test]
    fn every_framework_kind_is_subject_and_event() {
        for kind in [
            INVITATION_RECEIVED,
            INVITATION_CLAIMED,
            MEMBERSHIP_ROLES_CHANGED,
            WEBHOOK_DELIVERY_FAILED,
        ] {
            let (subject, event) = kind.split_once('.').expect("a kind carries one dot");
            assert!(!subject.is_empty(), "{kind} names a subject");
            assert!(!event.is_empty(), "{kind} names an event");
            assert!(
                kind.chars().all(|character| {
                    character.is_ascii_lowercase() || character == '_' || character == '.'
                }),
                "{kind} is lowercase snake case, so a locale key can be built from it",
            );
        }
    }

    #[test]
    fn hrefs_are_application_paths_rather_than_urls() {
        let id = Uuid::new_v4();

        for href in [
            team_settings_href(id),
            team_developers_href(id),
            organization_settings_href(id),
        ] {
            assert!(href.starts_with('/'), "{href} must be a path");
            assert!(
                href.contains(&id.to_string()),
                "{href} must name the tenant"
            );
        }
    }
}
