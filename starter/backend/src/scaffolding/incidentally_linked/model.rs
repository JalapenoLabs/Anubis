//! The `IncidentalLinkage` join model: creative concepts and peripheral notions.
//!
//! A join model is infrastructure rather than a page: it owns no name, no
//! description, and no UI of its own. What it does own is every rule the
//! has-many-through association needs, in one place, so both the association's
//! own endpoints and the `<name>_ids` field an `anubis scaffold field` run adds
//! to the owning model read the same definitions.

use std::collections::HashMap;

use anubis::http::ApiError;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::scaffolding::merely_peripheral::PeripheralNotion;
use crate::schema::{incidental_linkages, peripheral_notions};

/// This join, then the model that owns the association, then the one it reaches.
///
/// An association field names only its target class, exactly as Bullet Train's
/// does, so `anubis scaffold field` finds the join backing
/// `peripheral_notion_ids:super_select{class_name=PeripheralNotion}` by looking
/// for this constant across the application's models. Keep it in step with the
/// columns below.
pub const JOIN: [&str; 3] = ["IncidentalLinkage", "CreativeConcept", "PeripheralNotion"];

/// One creative concept linked to one peripheral notion.
///
/// The pair is unique in the database, so a link either exists or does not;
/// attaching twice is a no-op rather than a duplicate.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = incidental_linkages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct IncidentalLinkage {
    /// Primary key.
    pub id: Uuid,
    /// The creative concept side of the link.
    pub creative_concept_id: Uuid,
    /// The peripheral notion side of the link.
    pub peripheral_notion_id: Uuid,
    /// When the link was created.
    pub created_at: DateTime<Utc>,
    /// When the link was last updated, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = incidental_linkages)]
struct NewIncidentalLinkage {
    creative_concept_id: Uuid,
    peripheral_notion_id: Uuid,
}

impl IncidentalLinkage {
    /// The peripheral notions this team may attach.
    ///
    /// One definition, two duties, exactly as `docs/scaffolding.md` requires:
    /// it fills the association's options endpoint and validates every id
    /// submitted through a form, so a request can never smuggle in another
    /// tenant's record.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn valid_peripheral_notions(
        connection: &mut AsyncPgConnection,
        team_id: Uuid,
    ) -> QueryResult<Vec<PeripheralNotion>> {
        peripheral_notions::table
            .filter(peripheral_notions::team_id.eq(team_id))
            .order(peripheral_notions::name.asc())
            .select(PeripheralNotion::as_select())
            .load(connection)
            .await
    }

    /// The peripheral notions attached to one creative concept.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn attached_peripheral_notions(
        connection: &mut AsyncPgConnection,
        creative_concept_id: Uuid,
    ) -> QueryResult<Vec<PeripheralNotion>> {
        incidental_linkages::table
            .inner_join(peripheral_notions::table)
            .filter(incidental_linkages::creative_concept_id.eq(creative_concept_id))
            .order(peripheral_notions::name.asc())
            .select(PeripheralNotion::as_select())
            .load(connection)
            .await
    }

    /// The attached notion ids of a page of creative concepts, keyed by concept.
    ///
    /// One query serves a whole page, which is what keeps a list endpoint that
    /// serializes an association from turning into a query per row.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn peripheral_notion_ids_by_creative_concept(
        connection: &mut AsyncPgConnection,
        creative_concept_ids: &[Uuid],
    ) -> QueryResult<HashMap<Uuid, Vec<Uuid>>> {
        let mut attached: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        if creative_concept_ids.is_empty() {
            return Ok(attached);
        }

        let links: Vec<(Uuid, Uuid)> = incidental_linkages::table
            .inner_join(peripheral_notions::table)
            .filter(incidental_linkages::creative_concept_id.eq_any(creative_concept_ids))
            .order(peripheral_notions::name.asc())
            .select((
                incidental_linkages::creative_concept_id,
                incidental_linkages::peripheral_notion_id,
            ))
            .load(connection)
            .await?;

        for (creative_concept_id, peripheral_notion_id) in links {
            attached
                .entry(creative_concept_id)
                .or_default()
                .push(peripheral_notion_id);
        }
        Ok(attached)
    }

    /// Attaches one peripheral notion of the same team, idempotently.
    ///
    /// # Errors
    /// Answers `400` when the notion belongs to another team, and `500` when a
    /// query fails.
    pub async fn attach(
        connection: &mut AsyncPgConnection,
        creative_concept_id: Uuid,
        team_id: Uuid,
        peripheral_notion_id: Uuid,
    ) -> Result<(), ApiError> {
        let valid = Self::valid_peripheral_notions(connection, team_id)
            .await
            .map_err(log_internal)?;
        if !valid.iter().any(|notion| notion.id == peripheral_notion_id) {
            return Err(ApiError::validation(
                "That peripheral notion is not available to this team.",
            ));
        }

        let inserted = diesel::insert_into(incidental_linkages::table)
            .values(NewIncidentalLinkage {
                creative_concept_id,
                peripheral_notion_id,
            })
            .execute(connection)
            .await;
        match inserted {
            Ok(_rows) => Ok(()),
            // The unique pair is what makes attaching twice a no-op, including
            // when two requests race.
            Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _details,
            )) => Ok(()),
            Err(error) => Err(log_internal(error)),
        }
    }

    /// Detaches one peripheral notion, reporting whether a link was removed.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the query fails.
    pub async fn detach(
        connection: &mut AsyncPgConnection,
        creative_concept_id: Uuid,
        peripheral_notion_id: Uuid,
    ) -> QueryResult<bool> {
        let removed = diesel::delete(
            incidental_linkages::table
                .filter(incidental_linkages::creative_concept_id.eq(creative_concept_id))
                .filter(incidental_linkages::peripheral_notion_id.eq(peripheral_notion_id)),
        )
        .execute(connection)
        .await?;
        Ok(removed > 0)
    }

    /// Makes the attached set exactly `requested`, in one transaction.
    ///
    /// Every submitted id is checked against
    /// [`valid_peripheral_notions`](Self::valid_peripheral_notions) first, then
    /// the links that are gone are deleted and the ones that are new are
    /// inserted, so a form submission can neither reach another tenant's
    /// records nor leave the set half-written.
    ///
    /// # Errors
    /// Answers `400` when an id belongs to another team, and `500` when a
    /// query fails.
    pub async fn replace_all(
        connection: &mut AsyncPgConnection,
        creative_concept_id: Uuid,
        team_id: Uuid,
        requested: &[Uuid],
    ) -> Result<(), ApiError> {
        let valid = Self::valid_peripheral_notions(connection, team_id)
            .await
            .map_err(log_internal)?;

        let mut wanted: Vec<Uuid> = Vec::with_capacity(requested.len());
        for peripheral_notion_id in requested {
            if !valid
                .iter()
                .any(|notion| notion.id == *peripheral_notion_id)
            {
                return Err(ApiError::validation(
                    "That peripheral notion is not available to this team.",
                ));
            }
            if !wanted.contains(peripheral_notion_id) {
                wanted.push(*peripheral_notion_id);
            }
        }

        connection
            .transaction::<(), diesel::result::Error, _>(async |connection| {
                let existing: Vec<Uuid> = incidental_linkages::table
                    .filter(incidental_linkages::creative_concept_id.eq(creative_concept_id))
                    .select(incidental_linkages::peripheral_notion_id)
                    .load(connection)
                    .await?;

                let removed: Vec<Uuid> = existing
                    .iter()
                    .filter(|peripheral_notion_id| !wanted.contains(peripheral_notion_id))
                    .copied()
                    .collect();
                if !removed.is_empty() {
                    diesel::delete(
                        incidental_linkages::table
                            .filter(
                                incidental_linkages::creative_concept_id.eq(creative_concept_id),
                            )
                            .filter(incidental_linkages::peripheral_notion_id.eq_any(removed)),
                    )
                    .execute(connection)
                    .await?;
                }

                let added: Vec<NewIncidentalLinkage> = wanted
                    .iter()
                    .filter(|peripheral_notion_id| !existing.contains(peripheral_notion_id))
                    .map(|peripheral_notion_id| NewIncidentalLinkage {
                        creative_concept_id,
                        peripheral_notion_id: *peripheral_notion_id,
                    })
                    .collect();
                if !added.is_empty() {
                    diesel::insert_into(incidental_linkages::table)
                        .values(&added)
                        .execute(connection)
                        .await?;
                }

                Ok(())
            })
            .await
            .map_err(log_internal)
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "incidental linkage query failed: {{error.message}}",
    );
    ApiError::internal()
}
