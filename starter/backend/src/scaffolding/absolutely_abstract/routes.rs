//! Account (session-authenticated) CRUD for `CreativeConcept`.
//!
//! Collection routes hang off the team, so they authorize with the framework's
//! [`TeamMember`] guard directly. Member routes are shallow (`/creative-
//! concepts/{id}`), so they resolve the ownership chain themselves through
//! [`CreativeConcept::load_for_member`] and authorize against the same
//! compiled [`RoleSet`]. Both paths answer `404` for records the caller cannot
//! reach, so an id probe cannot tell a missing record from another tenant's.
//!
//! | Method | Path |
//! |---|---|
//! | GET, POST | `/account/teams/{team_id}/creative-concepts` |
//! | GET, PATCH, DELETE | `/account/creative-concepts/{creative_concept_id}` |

use anubis::auth::CurrentUser;
use anubis::db::DbPool;
use anubis::guard::TeamMember;
use anubis::http::{ApiError, ListParams, Pagination};
use anubis::roles::{Action, RoleSet};
use anubis::tenancy::TeamMembership;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::{CreativeConcept, CreativeConceptChanges, MODEL, NewCreativeConcept, SORTABLE};
use crate::schema::creative_concepts;

/// Returns the creative concept routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route("/teams/{team_id}/creative-concepts", get(list).post(create))
        .route(
            "/creative-concepts/{creative_concept_id}",
            get(show).patch(update).delete(destroy),
        )
        .with_state(CreativeConceptState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser and TeamMember resolve their dependencies from these
        // request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

#[derive(Clone)]
struct CreativeConceptState {
    pool: DbPool,
    roles: RoleSet,
}

/// The list endpoint's filters, whitelisted per model by the scaffolder.
#[derive(Debug, Deserialize)]
struct CreativeConceptFilters {
    /// Case-insensitive substring match on the name.
    name: Option<String>,
}

#[derive(Deserialize)]
struct CreateCreativeConceptBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
    // 🐺 anubis:create-body
}

#[derive(Deserialize)]
struct UpdateCreativeConceptBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
    // 🐺 anubis:update-body
}

/// The record as the account endpoints serialize it.
///
/// `serde(flatten)` keeps the wire shape identical to the record's own columns,
/// so a model with no associations serializes exactly as its table does. An
/// association `anubis scaffold field` adds lands its ids here, which is what
/// lets one form read and write the same shape.
#[derive(Serialize)]
struct CreativeConceptView {
    #[serde(flatten)]
    creative_concept: CreativeConcept,
    // 🐺 anubis:view-fields
}

impl CreativeConceptView {
    /// Builds the views for a page of records, one query per association.
    ///
    /// A model with no associations does no work here, which is why the lints
    /// below are allowed rather than expected: each one stops applying the
    /// moment an association is scaffolded onto this model.
    #[allow(
        clippy::unnecessary_wraps,
        clippy::unused_async,
        unused_variables,
        reason = "a scaffolded association queries through `connection` and can fail"
    )]
    async fn load(
        connection: &mut AsyncPgConnection,
        records: Vec<CreativeConcept>,
    ) -> QueryResult<Vec<Self>> {
        // 🐺 anubis:view-load
        Ok(records
            .into_iter()
            .map(|record| Self {
                // 🐺 anubis:view-values
                creative_concept: record,
            })
            .collect())
    }

    /// Builds the view for one record.
    async fn one(connection: &mut AsyncPgConnection, record: CreativeConcept) -> QueryResult<Self> {
        let mut views = Self::load(connection, vec![record]).await?;
        Ok(views.pop().expect("load yields one view per record"))
    }
}

#[derive(Serialize)]
struct CreativeConceptBody {
    creative_concept: CreativeConceptView,
}

#[derive(Serialize)]
struct CreativeConceptsBody {
    creative_concepts: Vec<CreativeConceptView>,
    pagination: Pagination,
}

async fn list(
    State(state): State<CreativeConceptState>,
    member: TeamMember,
    Query(params): Query<ListParams>,
    Query(filters): Query<CreativeConceptFilters>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = creative_concepts::table
        .filter(creative_concepts::team_id.eq(member.team.id))
        .into_boxed();
    if let Some(pattern) = pattern.clone() {
        counted = counted.filter(creative_concepts::name.ilike(pattern));
    }
    let total_items: i64 = counted
        .count()
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut page = creative_concepts::table
        .filter(creative_concepts::team_id.eq(member.team.id))
        .into_boxed();
    if let Some(pattern) = pattern {
        page = page.filter(creative_concepts::name.ilike(pattern));
    }

    // The trailing id keeps paging stable when a sort key ties.
    let (field, descending) = params.sort(&SORTABLE, "created_at");
    page = match (field, descending) {
        ("name", false) => page.order(creative_concepts::name.asc()),
        ("name", true) => page.order(creative_concepts::name.desc()),
        ("updated_at", false) => page.order(creative_concepts::updated_at.asc()),
        ("updated_at", true) => page.order(creative_concepts::updated_at.desc()),
        (_, false) => page.order(creative_concepts::created_at.asc()),
        (_, true) => page.order(creative_concepts::created_at.desc()),
    }
    .then_order_by(creative_concepts::id.asc());

    let records: Vec<CreativeConcept> = page
        .limit(params.limit())
        .offset(params.offset())
        .select(CreativeConcept::as_select())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;
    let creative_concepts = CreativeConceptView::load(&mut connection, records)
        .await
        .map_err(log_internal)?;

    Ok(Json(CreativeConceptsBody {
        creative_concepts,
        pagination: Pagination::new(&params, total_items),
    }))
}

async fn create(
    State(state): State<CreativeConceptState>,
    member: TeamMember,
    Json(body): Json<CreateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Create, MODEL)?;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("Name the creative concept."));
    }
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // 🐺 anubis:create-normalize

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let record: CreativeConcept = diesel::insert_into(creative_concepts::table)
        .values(NewCreativeConcept {
            // The team comes from the route, never from the body.
            team_id: member.team.id,
            name,
            description,
            // 🐺 anubis:insert-values
        })
        .returning(CreativeConcept::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;
    // 🐺 anubis:create-associations

    let creative_concept = CreativeConceptView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok((
        StatusCode::CREATED,
        Json(CreativeConceptBody { creative_concept }),
    ))
}

async fn show(
    State(state): State<CreativeConceptState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, membership) = load(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let creative_concept = CreativeConceptView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(CreativeConceptBody { creative_concept }))
}

async fn update(
    State(state): State<CreativeConceptState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<UpdateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, membership) = load(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::validation("Name the creative concept.")),
        other => other.map(str::to_owned),
    };
    // A blank description clears the column, which is what the form submits
    // when the user empties the field.
    let description = optional_text(body.description.as_deref());
    // 🐺 anubis:update-normalize

    // Associations are reconciled before the columns, so a request that only
    // changes an association still takes effect.
    // 🐺 anubis:update-associations

    let changes = CreativeConceptChanges {
        name,
        description,
        // 🐺 anubis:changeset-values
    };
    if changes.is_empty() {
        let creative_concept = CreativeConceptView::one(&mut connection, record)
            .await
            .map_err(log_internal)?;
        return Ok(Json(CreativeConceptBody { creative_concept }));
    }

    let updated: CreativeConcept =
        diesel::update(creative_concepts::table.filter(creative_concepts::id.eq(record.id)))
            .set(changes)
            .returning(CreativeConcept::as_returning())
            .get_result(&mut connection)
            .await
            .map_err(log_internal)?;

    let creative_concept = CreativeConceptView::one(&mut connection, updated)
        .await
        .map_err(log_internal)?;
    Ok(Json(CreativeConceptBody { creative_concept }))
}

async fn destroy(
    State(state): State<CreativeConceptState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Destroy)?;

    diesel::delete(creative_concepts::table.filter(creative_concepts::id.eq(creative_concept.id)))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Loads the creative concept, answering `404` when it is absent or unreachable.
async fn load(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    creative_concept_id: Uuid,
) -> Result<(CreativeConcept, TeamMembership), ApiError> {
    CreativeConcept::load_for_member(connection, user_id, creative_concept_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Rejects with `403` unless a held role grants the action on this model.
fn require(roles: &RoleSet, membership: &TeamMembership, action: Action) -> Result<(), ApiError> {
    if roles.can(&membership.roles, action, MODEL) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You do not have permission to do that.",
        ))
    }
}

/// Normalizes a submitted text value into a changeset column.
///
/// Absent leaves the column alone, blank clears it, and anything else stores
/// it trimmed. Every nullable text column an `anubis scaffold field` run adds
/// is normalized through here, so the rule is written once.
#[expect(
    clippy::option_option,
    reason = "Diesel's changeset shape for a nullable column"
)]
fn optional_text(submitted: Option<&str>) -> Option<Option<String>> {
    submitted.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

/// Builds a contains-pattern for `ILIKE`, escaping the wildcards so a search
/// for `50%` finds the literal text.
fn like_pattern(filter: Option<&str>) -> Option<String> {
    let trimmed = filter.map(str::trim).filter(|value| !value.is_empty())?;
    let escaped = trimmed
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("%{escaped}%"))
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "creative concept request failed: {{error.message}}",
    );
    ApiError::internal()
}
