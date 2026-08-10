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

/// A creative concept: the parent template, owned by a team.
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
    /// When the concept was created.
    pub created_at: DateTime<Utc>,
    /// When the concept was last updated, maintained by the database trigger.
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
}

/// The updatable shape; `None` leaves a column untouched.
#[derive(Debug, AsChangeset)]
#[diesel(table_name = creative_concepts)]
pub struct CreativeConceptChanges<'a> {
    /// New display name.
    pub name: Option<&'a str>,
}

impl CreativeConcept {
    /// Loads one concept the user may see, walking the chain to its team.
    ///
    /// Returns the concept together with the caller's membership in the owning
    /// team, or `None` when the concept does not exist *or* the caller is not
    /// a member of its team. Handlers answer both with `404`, so probing ids
    /// reveals nothing about other tenants.
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
        concept_id: Uuid,
    ) -> QueryResult<Option<(Self, TeamMembership)>> {
        let concept: Option<Self> = creative_concepts::table
            .filter(creative_concepts::id.eq(concept_id))
            .select(Self::as_select())
            .first(connection)
            .await
            .optional()?;
        let Some(concept) = concept else {
            return Ok(None);
        };

        let membership = TeamMembership::for_user(connection, user_id, concept.team_id).await?;
        Ok(membership.map(|membership| (concept, membership)))
    }
}
