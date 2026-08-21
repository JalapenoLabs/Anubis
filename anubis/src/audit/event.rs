//! The parts of one audit event: its context, its description, and its row.
//!
//! [`Context`] is what the request supplies (who acted, under which request
//! id), [`Event`] is what the handler describes (the verb, the subject, what
//! moved), and [`AuditEvent`] is what comes back out of the table.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::api::PlatformApplication;
use crate::auth::User;
use crate::http::ApiError;
use crate::schema::audit_events;
use crate::server::RequestId;

/// Keys the database maintains rather than a person.
///
/// A change set exists to answer "what did somebody change", and a row's own
/// timestamps answer "when". Leaving them in would put an `updated_at` diff on
/// every single update, which is noise in front of the answer.
const HOUSEKEEPING: [&str; 2] = ["created_at", "updated_at"];

/// The ambient facts a recorded event picks up from the request it happened in.
///
/// A handler takes it as an extractor, attributes it to whoever acted, and
/// hands it to [`record`](super::record):
///
/// ```ignore
/// async fn create(
///     member: TeamMember,
///     context: audit::Context,
///     Json(body): Json<CreateBody>,
/// ) -> Result<impl IntoResponse, ApiError> {
///     insert_record(&mut connection, member.team.id, body, &context.by(&member.user)).await
/// }
/// ```
///
/// Extraction never fails. Outside a served request there is no request id (the
/// middleware that mints one lives in [`crate::server`]), and an event without
/// one is still the truth about what happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    request_id: Option<String>,
    actor_id: Option<Uuid>,
    actor_name: Option<String>,
}

impl Context {
    /// The context of an act nobody performed: a job, a migration, a sweep.
    ///
    /// The row keeps a null actor, which is what the column is nullable for.
    /// The audit log's readers render it as the system rather than as a person
    /// whose name went missing.
    #[must_use]
    pub fn system() -> Self {
        Self::default()
    }

    /// This context, attributed to `user`.
    ///
    /// Returns a new context rather than mutating, so one extracted value
    /// serves every event a handler records.
    #[must_use]
    pub fn by(&self, user: &User) -> Self {
        Self {
            request_id: self.request_id.clone(),
            actor_id: Some(user.id),
            actor_name: Some(display_name(user)),
        }
    }

    /// This context, attributed to a platform application's token.
    ///
    /// A token belongs to its team rather than to any member (see
    /// [`crate::api::v1::ApiCaller`]), so the actor is the application's name
    /// and no user id: naming a person who was not there would be worse than
    /// naming nobody.
    #[must_use]
    pub fn by_application(&self, application: &PlatformApplication) -> Self {
        Self {
            request_id: self.request_id.clone(),
            actor_id: None,
            actor_name: Some(application.name.clone()),
        }
    }

    pub(super) fn actor_id(&self) -> Option<Uuid> {
        self.actor_id
    }

    pub(super) fn actor_name(&self) -> Option<&str> {
        self.actor_name.as_deref()
    }

    pub(super) fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }
}

impl<S> FromRequestParts<S> for Context
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            request_id: parts
                .extensions
                .get::<RequestId>()
                .map(|id| id.as_str().to_owned()),
            actor_id: None,
            actor_name: None,
        })
    }
}

/// How a person reads in a log: their name if they gave one, their email if not.
///
/// Takes the three columns rather than a [`User`], so a surface that needs a
/// label for somebody other than the caller selects those three instead of
/// loading a whole account. Blank names count as absent, because the profile
/// form submits one to clear a name and a row that reads as an empty string
/// names nobody.
///
/// # Examples
/// ```
/// use anubis::audit::person_label;
///
/// assert_eq!(person_label(Some("Ada"), Some("Lovelace"), "ada@example.com"), "Ada Lovelace");
/// assert_eq!(person_label(Some("Ada"), None, "ada@example.com"), "Ada");
/// assert_eq!(person_label(None, Some(" "), "ada@example.com"), "ada@example.com");
/// ```
#[must_use]
pub fn person_label(first_name: Option<&str>, last_name: Option<&str>, email: &str) -> String {
    let first = first_name.map(str::trim).filter(|name| !name.is_empty());
    let last = last_name.map(str::trim).filter(|name| !name.is_empty());

    match (first, last) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(only), None) | (None, Some(only)) => only.to_owned(),
        (None, None) => email.to_owned(),
    }
}

fn display_name(user: &User) -> String {
    person_label(
        user.first_name.as_deref(),
        user.last_name.as_deref(),
        &user.email,
    )
}

/// One act, described and ready to be recorded.
///
/// Built from the verb and the model it happened to, then narrowed by whatever
/// the handler knows: where it happened, which record, what that record is
/// called, and what moved.
///
/// ```ignore
/// Event::updated("CreativeConcept", record.id)
///     .team(record.team_id)
///     .label(&record.name)
///     .changes(Changes::between(&before, &after)?)
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Event<'a> {
    action: &'a str,
    subject_type: &'a str,
    subject_id: Option<Uuid>,
    subject_label: Option<&'a str>,
    team_id: Option<Uuid>,
    organization_id: Option<Uuid>,
    changes: Changes,
}

impl<'a> Event<'a> {
    /// An act of `action` upon a record of `subject_type`.
    ///
    /// `action` is one of this module's framework verbs for a framework
    /// surface, or one of [`crate::webhooks::LIFECYCLE_ACTIONS`] for a
    /// scaffolded model. `subject_type` is the model's name as `roles.yml`
    /// spells it.
    #[must_use]
    pub fn new(action: &'a str, subject_type: &'a str) -> Self {
        Self {
            action,
            subject_type,
            subject_id: None,
            subject_label: None,
            team_id: None,
            organization_id: None,
            changes: Changes::new(),
        }
    }

    /// A record of `subject_type` was created.
    #[must_use]
    pub fn created(subject_type: &'a str, subject_id: Uuid) -> Self {
        Self::new("created", subject_type).subject(subject_id)
    }

    /// A record of `subject_type` was updated.
    #[must_use]
    pub fn updated(subject_type: &'a str, subject_id: Uuid) -> Self {
        Self::new("updated", subject_type).subject(subject_id)
    }

    /// A record of `subject_type` was destroyed.
    #[must_use]
    pub fn destroyed(subject_type: &'a str, subject_id: Uuid) -> Self {
        Self::new("destroyed", subject_type).subject(subject_id)
    }

    /// Places the act inside a team.
    ///
    /// At most one of [`Event::team`] and [`Event::organization`] applies: an
    /// act belongs to the tenant it happened in, and an account-level act such
    /// as a password change belongs to neither.
    #[must_use]
    pub fn team(mut self, team_id: Uuid) -> Self {
        self.team_id = Some(team_id);
        self
    }

    /// Places the act inside an organization. See [`Event::team`].
    #[must_use]
    pub fn organization(mut self, organization_id: Uuid) -> Self {
        self.organization_id = Some(organization_id);
        self
    }

    /// Names the record the act was performed upon.
    #[must_use]
    pub fn subject(mut self, subject_id: Uuid) -> Self {
        self.subject_id = Some(subject_id);
        self
    }

    /// Copies how the subject read at the moment it was acted upon.
    ///
    /// Copied rather than joined on read, so a destroyed record still says what
    /// it was called.
    #[must_use]
    pub fn label(mut self, subject_label: &'a str) -> Self {
        self.subject_label = Some(subject_label);
        self
    }

    /// Attaches the fields that moved.
    #[must_use]
    pub fn changes(mut self, changes: Changes) -> Self {
        self.changes = changes;
        self
    }

    pub(super) fn action(&self) -> &str {
        self.action
    }

    pub(super) fn subject_type(&self) -> &str {
        self.subject_type
    }

    pub(super) fn subject_id(&self) -> Option<Uuid> {
        self.subject_id
    }

    pub(super) fn subject_label(&self) -> Option<&str> {
        self.subject_label
    }

    pub(super) fn team_id(&self) -> Option<Uuid> {
        self.team_id
    }

    pub(super) fn organization_id(&self) -> Option<Uuid> {
        self.organization_id
    }

    pub(super) fn change_set(&self) -> &Changes {
        &self.changes
    }
}

/// The fields one act moved, as `{"field": {"old": ..., "new": ...}}`.
///
/// Secrets are absent because nothing ever puts one here: [`Changes::between`]
/// diffs the serialized record, which is the shape the REST API answers with,
/// and the framework's own credential events record an empty set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Changes(Map<String, Value>);

impl Changes {
    /// An empty change set, which is what a create and a destroy record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one field's move to the set.
    ///
    /// # Examples
    /// ```
    /// let changes = anubis::audit::Changes::new().field("name", "Old", "New");
    /// assert!(!changes.is_empty());
    /// ```
    #[must_use]
    pub fn field(mut self, name: &str, old: impl Into<Value>, new: impl Into<Value>) -> Self {
        self.0.insert(
            name.to_owned(),
            Value::Object(Map::from_iter([
                ("old".to_owned(), old.into()),
                ("new".to_owned(), new.into()),
            ])),
        );
        self
    }

    /// The fields that differ between two serializations of the same record.
    ///
    /// This is what a scaffolded model records on an update, and it is why no
    /// generated model needs per-field audit code: the diff is taken from the
    /// record itself, so a column added by `anubis scaffold field` is audited
    /// the moment it exists. The timestamps the database maintains are left
    /// out, since they move on every update and answer a question the row's own
    /// `created_at` already answers.
    ///
    /// Two values that do not serialize as JSON objects yield an empty set and
    /// a logged error, rather than failing the write they belong to: the caller
    /// is inside the transaction that performed the write, and losing the write
    /// would be a far worse outcome than losing its change set.
    ///
    /// # Errors
    /// Returns [`diesel::result::Error::SerializationError`] when either value
    /// will not render as JSON at all.
    pub fn between(before: &impl Serialize, after: &impl Serialize) -> QueryResult<Self> {
        let before = to_json(before)?;
        let after = to_json(after)?;

        let (Value::Object(before), Value::Object(after)) = (before, after) else {
            tracing::error!(
                "audit change sets compare serialized records; \
                 a value that is not a JSON object recorded no changes",
            );
            return Ok(Self::new());
        };

        let mut changes = Map::new();
        for (field, new) in after {
            if HOUSEKEEPING.contains(&field.as_str()) {
                continue;
            }
            let old = before.get(&field).unwrap_or(&Value::Null);
            if old == &new {
                continue;
            }
            changes.insert(
                field,
                Value::Object(Map::from_iter([
                    ("old".to_owned(), old.clone()),
                    ("new".to_owned(), new),
                ])),
            );
        }

        Ok(Self(changes))
    }

    /// Returns `true` when nothing moved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn as_value(&self) -> Value {
        Value::Object(self.0.clone())
    }
}

fn to_json(value: &impl Serialize) -> QueryResult<Value> {
    serde_json::to_value(value)
        .map_err(|source| diesel::result::Error::SerializationError(Box::new(source)))
}

/// One recorded act, as the listing endpoints serve it.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = audit_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AuditEvent {
    /// Primary key.
    pub id: Uuid,
    /// The team the act happened in, when it happened in one.
    pub team_id: Option<Uuid>,
    /// The organization the act happened in, when it happened in one.
    pub organization_id: Option<Uuid>,
    /// Who acted; null for the system and for a since-deleted account.
    pub user_id: Option<Uuid>,
    /// How the actor read at the moment they acted.
    pub actor_name: Option<String>,
    /// The verb: a lifecycle action, or one of the framework's dotted verbs.
    pub action: String,
    /// The model the act was performed upon.
    pub subject_type: String,
    /// The record acted upon, when the act named one.
    pub subject_id: Option<Uuid>,
    /// How the subject read at the moment it was acted upon.
    pub subject_label: Option<String>,
    /// The fields that moved, as `{"field": {"old": ..., "new": ...}}`.
    pub changes: Value,
    /// The `x-request-id` of the request that did this.
    pub request_id: Option<String>,
    /// When the act was recorded.
    pub created_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and the timestamp.
#[derive(Insertable)]
#[diesel(table_name = audit_events)]
pub(super) struct NewAuditEvent<'a> {
    pub team_id: Option<Uuid>,
    pub organization_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub actor_name: Option<&'a str>,
    pub action: &'a str,
    pub subject_type: &'a str,
    pub subject_id: Option<Uuid>,
    pub subject_label: Option<&'a str>,
    pub changes: Value,
    pub request_id: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Changes;

    #[test]
    fn a_diff_names_only_the_fields_that_moved() {
        let before = json!({ "name": "Draft", "description": null, "count": 1 });
        let after = json!({ "name": "Final", "description": "Ready", "count": 1 });

        let changes = Changes::between(&before, &after).expect("objects must diff");
        let rendered = changes.as_value();

        assert_eq!(rendered["name"], json!({ "old": "Draft", "new": "Final" }));
        assert_eq!(
            rendered["description"],
            json!({ "old": null, "new": "Ready" }),
        );
        assert!(rendered.get("count").is_none(), "{rendered}");
    }

    #[test]
    fn the_databases_own_timestamps_are_not_changes() {
        let before = json!({ "name": "Same", "updated_at": "2026-01-01T00:00:00Z" });
        let after = json!({ "name": "Same", "updated_at": "2026-01-02T00:00:00Z" });

        let changes = Changes::between(&before, &after).expect("objects must diff");

        assert!(changes.is_empty(), "{:?}", changes.as_value());
    }

    #[test]
    fn a_value_that_is_not_a_record_records_no_changes() {
        let changes = Changes::between(&"before", &"after").expect("a scalar must not fail");

        assert!(changes.is_empty(), "{:?}", changes.as_value());
    }

    #[test]
    fn a_field_carries_both_sides() {
        let changes = Changes::new()
            .field("roles", vec!["default"], vec!["admin"])
            .field("name", "Old", "New");

        assert_eq!(
            changes.as_value()["roles"],
            json!({ "old": [ "default" ], "new": [ "admin" ] }),
        );
        assert_eq!(
            changes.as_value()["name"],
            json!({ "old": "Old", "new": "New" })
        );
    }
}
