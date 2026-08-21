//! The `GranularDetail` model: a record owned two parents away from its team.

use anubis::tenancy::TeamMembership;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::scaffolding::absolutely_abstract::CreativeConcept;
use crate::scaffolding::completely_concrete::TangibleThing;
use crate::schema::{creative_concepts, granular_details, tangible_things};

/// The key this model is granted permissions under in `config/roles.yml`.
pub const MODEL: &str = "GranularDetail";

/// Fields the list endpoint accepts in `?sort=`.
pub const SORTABLE: [&str; 3] = ["name", "created_at", "updated_at"];

/// A granular detail, owned through its tangible thing.
///
/// The record carries only its immediate parent, exactly as a nested model
/// does. The team it belongs to is never denormalized onto the row: it is read
/// off the chain's root in the same query that loads the record, so a tenant
/// can never drift out of step with the parent that decides it.
///
/// `ToSchema` is what puts the record in the `/api/v1` OpenAPI document: the
/// view the handlers serialize flattens this struct, so a column added by
/// `anubis scaffold field` reaches the published contract with no second
/// declaration to keep in step.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable, ToSchema)]
#[diesel(table_name = granular_details)]
#[diesel(check_for_backend(diesel::pg::Pg))]
// The doc comment above is for whoever reads this code; the description below
// is what an API consumer reads in the published document.
#[schema(description = "A granular detail, owned through its tangible thing.")]
pub struct GranularDetail {
    /// Primary key.
    pub id: Uuid,
    /// The parent tangible thing; the ownership chain continues through it.
    pub tangible_thing_id: Uuid,
    /// Display name.
    pub name: String,
    /// Optional long-form detail.
    pub description: Option<String>,
    // 🐺 anubis:record-fields
    /// When the granular detail was created.
    pub created_at: DateTime<Utc>,
    /// When the granular detail was last updated, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = granular_details)]
pub struct NewGranularDetail<'a> {
    /// The parent tangible thing, taken from the route and tenancy-checked.
    pub tangible_thing_id: Uuid,
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
#[diesel(table_name = granular_details)]
#[expect(
    clippy::option_option,
    reason = "Diesel's changeset shape for a nullable column"
)]
pub struct GranularDetailChanges {
    /// New display name.
    pub name: Option<String>,
    /// New description, or `Some(None)` to clear it.
    pub description: Option<Option<String>>,
    /// A different parent tangible thing, already checked against
    /// [`GranularDetail::valid_tangible_things`].
    pub tangible_thing_id: Option<Uuid>,
    // 🐺 anubis:changeset-fields
}

impl GranularDetailChanges {
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
        if self.tangible_thing_id.is_some() {
            return false;
        }
        // 🐺 anubis:changeset-empty
        true
    }
}

impl GranularDetail {
    /// The tangible things this granular detail may belong to.
    ///
    /// One definition, two duties, exactly as `docs/scaffolding.md` requires:
    /// it populates the parent select field and validates a submitted
    /// `tangible_thing_id` on write, so a form can never smuggle in another
    /// tenant's record. The parent is not team-owned, so the scope is a join
    /// through the chain's root rather than a comparison.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn valid_tangible_things(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
    ) -> QueryResult<Vec<TangibleThing>> {
        tangible_things::table
            .inner_join(creative_concepts::table)
            .filter(creative_concepts::team_id.eq(team_id))
            .order(tangible_things::name.asc())
            .select(TangibleThing::as_select())
            .load(connection)
            .await
    }

    /// Loads one granular detail the user may see, walking the chain to its team.
    ///
    /// Returns the granular detail, its parent tangible thing, the creative
    /// concept at the chain's root, and the caller's membership in the owning
    /// team, or `None` when the record does not exist *or* the caller is not a
    /// member of that team. Handlers answer both with `404`.
    ///
    /// The chain's root is selected alongside the parent rather than
    /// denormalized onto the row, which is the whole reason a third ownership
    /// level costs one join and no trigger. The application's three tables
    /// join in one query; the membership is a second lookup, because
    /// application and framework tables cannot share a Diesel query (see the
    /// note in `crate::schema`).
    ///
    /// # Errors
    /// Returns the underlying Diesel error when either query fails.
    pub async fn load_for_member(
        connection: &mut AsyncPgConnection,
        user_id: Uuid,
        granular_detail_id: Uuid,
    ) -> QueryResult<Option<(Self, TangibleThing, CreativeConcept, TeamMembership)>> {
        let found: Option<(Self, TangibleThing, CreativeConcept)> = granular_details::table
            .inner_join(tangible_things::table.inner_join(creative_concepts::table))
            .filter(granular_details::id.eq(granular_detail_id))
            .select((
                Self::as_select(),
                TangibleThing::as_select(),
                CreativeConcept::as_select(),
            ))
            .first(connection)
            .await
            .optional()?;
        let Some((granular_detail, tangible_thing, creative_concept)) = found else {
            return Ok(None);
        };

        let membership =
            TeamMembership::for_user(connection, user_id, creative_concept.team_id).await?;
        Ok(membership.map(|membership| {
            (
                granular_detail,
                tangible_thing,
                creative_concept,
                membership,
            )
        }))
    }

    /// Loads one granular detail owned by `team_id`, for the `/api/v1` handlers.
    ///
    /// A platform token authenticates as its team rather than as a user, so the
    /// chain ends at the root's `team_id` in the same query. `None` covers both
    /// a missing record and another tenant's, which is why the API answers
    /// `404` for either.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn load_for_team(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
        granular_detail_id: Uuid,
    ) -> QueryResult<Option<(Self, TangibleThing, CreativeConcept)>> {
        granular_details::table
            .inner_join(tangible_things::table.inner_join(creative_concepts::table))
            .filter(granular_details::id.eq(granular_detail_id))
            .filter(creative_concepts::team_id.eq(team_id))
            .select((
                Self::as_select(),
                TangibleThing::as_select(),
                CreativeConcept::as_select(),
            ))
            .first(connection)
            .await
            .optional()
    }

    // 🐺 anubis:model-methods
}
