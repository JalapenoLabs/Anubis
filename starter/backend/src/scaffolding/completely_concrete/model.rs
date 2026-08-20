//! The `TangibleThing` model: a record owned through its creative concept.

use anubis::tenancy::TeamMembership;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::scaffolding::absolutely_abstract::CreativeConcept;
use crate::schema::{creative_concepts, tangible_things};

/// The key this model is granted permissions under in `config/roles.yml`.
pub const MODEL: &str = "TangibleThing";

/// Fields the list endpoint accepts in `?sort=`.
pub const SORTABLE: [&str; 3] = ["name", "created_at", "updated_at"];

/// A tangible thing, owned through its creative concept.
///
/// `ToSchema` is what puts the record in the `/api/v1` OpenAPI document: the
/// view the handlers serialize flattens this struct, so a column added by
/// `anubis scaffold field` reaches the published contract with no second
/// declaration to keep in step.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable, ToSchema)]
#[diesel(table_name = tangible_things)]
#[diesel(check_for_backend(diesel::pg::Pg))]
// The doc comment above is for whoever reads this code; the description below
// is what an API consumer reads in the published document.
#[schema(description = "A tangible thing, owned through its creative concept.")]
pub struct TangibleThing {
    /// Primary key.
    pub id: Uuid,
    /// The parent creative concept; the ownership chain continues through it.
    pub creative_concept_id: Uuid,
    /// Display name.
    pub name: String,
    /// Optional long-form detail.
    pub description: Option<String>,
    // 🐺 anubis:record-fields
    /// When the tangible thing was created.
    pub created_at: DateTime<Utc>,
    /// When the tangible thing was last updated, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = tangible_things)]
pub struct NewTangibleThing<'a> {
    /// The parent creative concept, taken from the route and tenancy-checked.
    pub creative_concept_id: Uuid,
    /// Display name.
    pub name: &'a str,
    /// Optional long-form detail.
    pub description: Option<&'a str>,
    // 🐺 anubis:insert-fields
}

/// The updatable shape; `None` leaves a column untouched.
///
/// `description` is doubly optional because the column is nullable: the outer
/// `None` means "unchanged" and `Some(None)` clears it. The fields are owned
/// rather than borrowed because that nesting reads as `&Option<&str>` inside
/// Diesel's generated changeset, which is a shape clippy rightly dislikes.
#[derive(Debug, AsChangeset)]
#[diesel(table_name = tangible_things)]
#[expect(
    clippy::option_option,
    reason = "Diesel's changeset shape for a nullable column"
)]
pub struct TangibleThingChanges {
    /// New display name.
    pub name: Option<String>,
    /// New description, or `Some(None)` to clear it.
    pub description: Option<Option<String>>,
    /// A different parent creative concept, already checked against
    /// [`TangibleThing::valid_creative_concepts`].
    pub creative_concept_id: Option<Uuid>,
    // 🐺 anubis:changeset-fields
}

impl TangibleThingChanges {
    /// Whether the request submitted no change at all.
    ///
    /// Diesel refuses an empty changeset, so the handler answers with the
    /// record untouched instead. One `if` per column keeps the anchor below in
    /// statement position, which is what lets `anubis scaffold field` extend
    /// the test with a new column.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        if self.name.is_some() {
            return false;
        }
        if self.description.is_some() {
            return false;
        }
        if self.creative_concept_id.is_some() {
            return false;
        }
        // 🐺 anubis:changeset-empty
        true
    }
}

impl TangibleThing {
    /// The creative concepts this tangible thing may belong to.
    ///
    /// One definition, two duties, exactly as `docs/scaffolding.md` requires:
    /// it populates the parent select field and validates a submitted
    /// `creative_concept_id` on write, so a form can never smuggle in another
    /// tenant's record.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn valid_creative_concepts(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
    ) -> QueryResult<Vec<CreativeConcept>> {
        creative_concepts::table
            .filter(creative_concepts::team_id.eq(team_id))
            .order(creative_concepts::name.asc())
            .select(CreativeConcept::as_select())
            .load(connection)
            .await
    }

    /// Loads one tangible thing the user may see, walking the chain to its team.
    ///
    /// Returns the tangible thing, its parent creative concept, and the
    /// caller's membership in the owning team, or `None` when the record does
    /// not exist *or* the caller is not a member of that team. Handlers answer
    /// both with `404`.
    ///
    /// The application's two tables join in one query; the membership is a
    /// second lookup, because application and framework tables cannot share a
    /// Diesel query (see the note in `crate::schema`).
    ///
    /// # Errors
    /// Returns the underlying Diesel error when either query fails.
    pub async fn load_for_member(
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        tangible_thing_id: Uuid,
    ) -> QueryResult<Option<(Self, CreativeConcept, TeamMembership)>> {
        let found: Option<(Self, CreativeConcept)> = tangible_things::table
            .inner_join(creative_concepts::table)
            .filter(tangible_things::id.eq(tangible_thing_id))
            .select((Self::as_select(), CreativeConcept::as_select()))
            .first(connection)
            .await
            .optional()?;
        let Some((tangible_thing, creative_concept)) = found else {
            return Ok(None);
        };

        let membership =
            TeamMembership::for_user(connection, user_id, creative_concept.team_id).await?;
        Ok(membership.map(|membership| (tangible_thing, creative_concept, membership)))
    }

    /// Loads one tangible thing owned by `team_id`, for the `/api/v1` handlers.
    ///
    /// A platform token authenticates as its team rather than as a user, so the
    /// chain ends at the parent's `team_id` in the same query. `None` covers
    /// both a missing record and another tenant's, which is why the API answers
    /// `404` for either.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn load_for_team(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
        tangible_thing_id: Uuid,
    ) -> QueryResult<Option<(Self, CreativeConcept)>> {
        tangible_things::table
            .inner_join(creative_concepts::table)
            .filter(tangible_things::id.eq(tangible_thing_id))
            .filter(creative_concepts::team_id.eq(team_id))
            .select((Self::as_select(), CreativeConcept::as_select()))
            .first(connection)
            .await
            .optional()
    }

    // 🐺 anubis:model-methods
}
