//! The `CreativeConcept` model: a record owned directly by a team.

use anubis::tenancy::TeamMembership;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::schema::creative_concepts;

/// The key this model is granted permissions under in `config/roles.yml`.
pub const MODEL: &str = "CreativeConcept";

/// Fields the list endpoint accepts in `?sort=`.
///
/// Anything else falls back to the default sort, so a caller can never order
/// by a column the model does not mean to expose.
pub const SORTABLE: [&str; 3] = ["name", "created_at", "updated_at"];

/// A creative concept, owned by a team.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable)]
#[diesel(table_name = creative_concepts)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CreativeConcept {
    /// Primary key.
    pub id: Uuid,
    /// The owning team; the whole ownership chain hangs off this column.
    pub team_id: Uuid,
    /// Display name.
    pub name: String,
    /// Optional long-form detail.
    pub description: Option<String>,
    /// When the creative concept was created.
    pub created_at: DateTime<Utc>,
    /// When the creative concept was last updated, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = creative_concepts)]
pub struct NewCreativeConcept<'a> {
    /// The owning team, taken from the route, never from the request body.
    pub team_id: Uuid,
    /// Display name.
    pub name: &'a str,
    /// Optional long-form detail.
    pub description: Option<&'a str>,
}

/// The updatable shape; `None` leaves a column untouched.
///
/// `description` is doubly optional because the column is nullable: the outer
/// `None` means "unchanged" and `Some(None)` clears it. The fields are owned
/// rather than borrowed because that nesting reads as `&Option<&str>` inside
/// Diesel's generated changeset, which is a shape clippy rightly dislikes.
#[derive(Debug, AsChangeset)]
#[diesel(table_name = creative_concepts)]
#[expect(
    clippy::option_option,
    reason = "Diesel's changeset shape for a nullable column"
)]
pub struct CreativeConceptChanges {
    /// New display name.
    pub name: Option<String>,
    /// New description, or `Some(None)` to clear it.
    pub description: Option<Option<String>>,
}

impl CreativeConcept {
    /// Loads one creative concept the user may see, walking the chain to its team.
    ///
    /// Returns the creative concept together with the caller's membership in
    /// the owning team, or `None` when the record does not exist *or* the
    /// caller is not a member of its team. Handlers answer both with `404`, so
    /// probing ids reveals nothing about other tenants.
    ///
    /// The membership lookup is a second query rather than a join: Rust's
    /// orphan rules stop an application crate from declaring its tables and
    /// the framework's in one Diesel query (see the note in `crate::schema`).
    ///
    /// # Errors
    /// Returns the underlying Diesel error when either query fails.
    pub async fn load_for_member(
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        creative_concept_id: Uuid,
    ) -> QueryResult<Option<(Self, TeamMembership)>> {
        let creative_concept: Option<Self> = creative_concepts::table
            .filter(creative_concepts::id.eq(creative_concept_id))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()?;
        let Some(creative_concept) = creative_concept else {
            return Ok(None);
        };

        let membership =
            TeamMembership::for_user(connection, user_id, creative_concept.team_id).await?;
        Ok(membership.map(|membership| (creative_concept, membership)))
    }
}
