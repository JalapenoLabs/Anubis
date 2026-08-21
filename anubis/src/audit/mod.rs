//! The audit log: who did what, recorded where the write happened.
//!
//! Bullet Train shipped audit logs as a Pro module; here they are framework
//! plumbing, on the same terms outgoing webhooks are. [`record`] writes one row
//! through the caller's own connection, so an audit event commits with the
//! write that caused it and a rolled-back write leaves nothing behind. Nothing
//! in the framework updates or deletes a row, and no endpoint offers either:
//! the log is append-only, which is the only property that makes it evidence.
//!
//! # Recording
//!
//! A handler picks up the ambient facts with the [`Context`] extractor, names
//! the actor, and hands both to [`record`]:
//!
//! ```ignore
//! async fn rename_team(
//!     member: TeamMember,
//!     context: audit::Context,
//!     Json(body): Json<NameBody>,
//! ) -> Result<impl IntoResponse, ApiError> {
//!     // ... perform the rename ...
//!     audit::record(
//!         &mut connection,
//!         &context.by(&member.user),
//!         &audit::Event::new(audit::TEAM_RENAMED, "Team")
//!             .team(member.team.id)
//!             .subject(member.team.id)
//!             .label(&name)
//!             .changes(Changes::new().field("name", old_name, name.clone())),
//!     )
//!     .await?;
//! }
//! ```
//!
//! # What a row says
//!
//! `action` is the verb. A scaffolded model records the bare lifecycle action
//! (`created`, `updated`, `destroyed`) and names itself in `subject_type`; the
//! framework's own surfaces record a dotted verb, and every one of them is a
//! constant in this module ([`TEAM_RENAMED`], [`MEMBER_ROLE_CHANGED`], and so
//! on).
//!
//! `team_id` and `organization_id` say where the act happened, and at most one
//! of them is set: a team-level act names its team, an organization-level act
//! names its organization, and an account-level act such as a password change
//! names neither, because it happens outside every tenant the account belongs
//! to.
//!
//! `actor_name` and `subject_label` are copied rather than joined. A log that
//! renders "(deleted user)" where a name belongs has lost the answer it exists
//! to give, so the row keeps how both read at the moment they were written.
//!
//! # Secrets
//!
//! [`Changes`] never carries one, and not because anything strips it
//! afterwards. A scaffolded model's change set is computed by
//! [`Changes::between`] from the serialized record, which is the same shape the
//! REST API answers with and so holds nothing secret to begin with; the
//! framework's own credential events record an empty change set, because the
//! fact that a password changed is the whole of what an auditor needs and the
//! password is not.
//!
//! # Reading
//!
//! [`router`] serves the team-scoped listing at
//! `/teams/{team_id}/audit-events`, gated on the admin role, and the caller's
//! own account events at `/audit-events`. Applications mount it under
//! `/account`. See `docs/audit.md`.

mod event;
mod routes;

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::audit_events;

#[doc(inline)]
pub use event::{AuditEvent, Changes, Context, Event, person_label};
#[doc(inline)]
pub use routes::router;

/// A team was created inside its organization.
pub const TEAM_CREATED: &str = "team.created";
/// A team was renamed.
pub const TEAM_RENAMED: &str = "team.renamed";
/// A team was dissolved, taking its records with it.
pub const TEAM_DESTROYED: &str = "team.destroyed";
/// An organization was created.
pub const ORGANIZATION_CREATED: &str = "organization.created";
/// An organization was renamed.
pub const ORGANIZATION_RENAMED: &str = "organization.renamed";
/// An organization was deleted, taking its teams with it.
pub const ORGANIZATION_DESTROYED: &str = "organization.destroyed";
/// Somebody joined a tenant by claiming their invitation.
pub const MEMBER_ADDED: &str = "member.added";
/// A member's roles were replaced with a new set.
pub const MEMBER_ROLE_CHANGED: &str = "member.role_changed";
/// An administrator removed somebody else from a tenant.
pub const MEMBER_REMOVED: &str = "member.removed";
/// Somebody left a tenant of their own accord.
pub const MEMBER_LEFT: &str = "member.left";
/// An invitation was sent.
pub const INVITATION_CREATED: &str = "invitation.created";
/// An invitation was accepted, which also adds the member.
pub const INVITATION_CLAIMED: &str = "invitation.claimed";
/// A pending invitation was taken back before it was claimed.
pub const INVITATION_REVOKED: &str = "invitation.revoked";
/// An account's password was rotated.
pub const PASSWORD_CHANGED: &str = "password.changed";
/// An account confirmed a TOTP enrollment.
pub const MFA_ENROLLED: &str = "mfa.enrolled";
/// An account turned its TOTP second factor off.
pub const MFA_DISABLED: &str = "mfa.disabled";
/// An account registered a passkey.
pub const PASSKEY_ADDED: &str = "passkey.added";
/// An account removed one of its passkeys.
pub const PASSKEY_REMOVED: &str = "passkey.removed";
/// An account signed one of its own sessions out.
pub const SESSION_REVOKED: &str = "session.revoked";
/// An account was deleted, along with the tenancy it left behind.
pub const ACCOUNT_DELETED: &str = "account.deleted";

/// Writes one audit event through `connection`, returning its id.
///
/// `context` says who acted and in which request; `event` says what happened.
/// Both are borrowed, because a handler recording two events in one request
/// reuses the same context.
///
/// The insert rides the caller's own connection, which is the point: recording
/// inside the transaction that performed the write means a rolled-back write
/// records nothing, and a committed write never loses its evidence. See
/// `docs/jobs.md` for the same argument about the job queue.
///
/// # Errors
/// Returns the underlying query error when the insert fails. Returning a query
/// error is deliberate: the caller is already inside a Diesel transaction, so
/// the failure joins the rollback of the write it belongs to rather than
/// needing an error type of its own.
///
/// # Examples
/// ```ignore
/// connection
///     .transaction::<_, diesel::result::Error, _>(async |connection| {
///         let record = insert_project(connection).await?;
///         anubis::audit::record(
///             connection,
///             &context,
///             &anubis::audit::Event::created("Project", record.id)
///                 .team(team_id)
///                 .label(&record.name),
///         )
///         .await?;
///         Ok(record)
///     })
///     .await?;
/// ```
pub async fn record(
    connection: &mut AsyncPgConnection,
    context: &Context,
    event: &Event<'_>,
) -> QueryResult<Uuid> {
    diesel::insert_into(audit_events::table)
        .values(event::NewAuditEvent {
            team_id: event.team_id(),
            organization_id: event.organization_id(),
            user_id: context.actor_id(),
            actor_name: context.actor_name(),
            action: event.action(),
            subject_type: event.subject_type(),
            subject_id: event.subject_id(),
            subject_label: event.subject_label(),
            changes: event.change_set().as_value(),
            request_id: context.request_id(),
        })
        .returning(audit_events::id)
        .get_result(connection)
        .await
}

#[cfg(test)]
mod tests {
    use super::{
        ACCOUNT_DELETED, MEMBER_ROLE_CHANGED, ORGANIZATION_RENAMED, PASSWORD_CHANGED, TEAM_RENAMED,
    };

    /// The framework's verbs are dotted, so a reader can tell one from the bare
    /// lifecycle action a scaffolded model records.
    #[test]
    fn every_framework_verb_is_a_dotted_pair() {
        for action in [
            TEAM_RENAMED,
            ORGANIZATION_RENAMED,
            MEMBER_ROLE_CHANGED,
            PASSWORD_CHANGED,
            ACCOUNT_DELETED,
        ] {
            let (noun, verb) = action
                .split_once('.')
                .unwrap_or_else(|| panic!("{action} must be <noun>.<verb>"));
            assert!(!noun.is_empty(), "{action}");
            assert!(!verb.is_empty(), "{action}");
            assert!(
                !verb.contains('.'),
                "{action} must carry exactly one separator",
            );
        }
    }
}
