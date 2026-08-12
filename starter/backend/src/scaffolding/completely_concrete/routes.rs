//! Account (session-authenticated) CRUD for `TangibleThing`.
//!
//! Every route resolves the ownership chain before it touches a record: the
//! collection routes through their parent creative concept, the member routes
//! through the tangible thing's own parent. A record the caller cannot reach
//! answers `404`, identically to one that does not exist.
//!
//! | Method | Path |
//! |---|---|
//! | GET, POST | `/account/creative-concepts/{creative_concept_id}/tangible-things` |
//! | GET, PATCH, DELETE | `/account/tangible-things/{tangible_thing_id}` |

use anubis::auth::CurrentUser;
use anubis::db::DbPool;
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

use super::model::{MODEL, NewTangibleThing, SORTABLE, TangibleThing, TangibleThingChanges};
use crate::scaffolding::absolutely_abstract::CreativeConcept;
use crate::schema::tangible_things;

/// Returns the tangible thing routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/creative-concepts/{creative_concept_id}/tangible-things",
            get(list).post(create),
        )
        .route(
            "/tangible-things/{tangible_thing_id}",
            get(show).patch(update).delete(destroy),
        )
        .with_state(TangibleThingState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser resolves its dependencies from these request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

#[derive(Clone)]
struct TangibleThingState {
    pool: DbPool,
    roles: RoleSet,
}

/// The list endpoint's filters, whitelisted per model by the scaffolder.
#[derive(Debug, Deserialize)]
struct TangibleThingFilters {
    /// Case-insensitive substring match on the name.
    name: Option<String>,
}

#[derive(Deserialize)]
struct CreateTangibleThingBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
    // 🐺 anubis:create-body
}

#[derive(Deserialize)]
struct UpdateTangibleThingBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
    /// Moves the tangible thing to another creative concept of the same team.
    creative_concept_id: Option<Uuid>,
    // 🐺 anubis:update-body
}

/// The record as the account endpoints serialize it.
///
/// `serde(flatten)` keeps the wire shape identical to the record's own columns,
/// so a model with no associations serializes exactly as its table does. An
/// association `anubis scaffold field` adds lands its ids here, which is what
/// lets one form read and write the same shape.
#[derive(Serialize)]
struct TangibleThingView {
    #[serde(flatten)]
    tangible_thing: TangibleThing,
    // 🐺 anubis:view-fields
}

impl TangibleThingView {
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
        records: Vec<TangibleThing>,
    ) -> QueryResult<Vec<Self>> {
        // 🐺 anubis:view-load
        Ok(records
            .into_iter()
            .map(|record| Self {
                // 🐺 anubis:view-values
                tangible_thing: record,
            })
            .collect())
    }

    /// Builds the view for one record.
    async fn one(connection: &mut AsyncPgConnection, record: TangibleThing) -> QueryResult<Self> {
        let mut views = Self::load(connection, vec![record]).await?;
        Ok(views.pop().expect("load yields one view per record"))
    }
}

#[derive(Serialize)]
struct TangibleThingBody {
    tangible_thing: TangibleThingView,
}

#[derive(Serialize)]
struct TangibleThingsBody {
    tangible_things: Vec<TangibleThingView>,
    pagination: Pagination,
}

async fn list(
    State(state): State<TangibleThingState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
    Query(params): Query<ListParams>,
    Query(filters): Query<TangibleThingFilters>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load_parent(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = tangible_things::table
        .filter(tangible_things::creative_concept_id.eq(creative_concept.id))
        .into_boxed();
    if let Some(pattern) = pattern.clone() {
        counted = counted.filter(tangible_things::name.ilike(pattern));
    }
    let total_items: i64 = counted
        .count()
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    let mut page = tangible_things::table
        .filter(tangible_things::creative_concept_id.eq(creative_concept.id))
        .into_boxed();
    if let Some(pattern) = pattern {
        page = page.filter(tangible_things::name.ilike(pattern));
    }

    // The trailing id keeps paging stable when a sort key ties.
    let (field, descending) = params.sort(&SORTABLE, "created_at");
    page = match (field, descending) {
        ("name", false) => page.order(tangible_things::name.asc()),
        ("name", true) => page.order(tangible_things::name.desc()),
        ("updated_at", false) => page.order(tangible_things::updated_at.asc()),
        ("updated_at", true) => page.order(tangible_things::updated_at.desc()),
        (_, false) => page.order(tangible_things::created_at.asc()),
        (_, true) => page.order(tangible_things::created_at.desc()),
    }
    .then_order_by(tangible_things::id.asc());

    let records: Vec<TangibleThing> = page
        .limit(params.limit())
        .offset(params.offset())
        .select(TangibleThing::as_select())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;
    let tangible_things = TangibleThingView::load(&mut connection, records)
        .await
        .map_err(log_internal)?;

    Ok(Json(TangibleThingsBody {
        tangible_things,
        pagination: Pagination::new(&params, total_items),
    }))
}

async fn create(
    State(state): State<TangibleThingState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<CreateTangibleThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load_parent(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Create)?;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("Name the tangible thing."));
    }
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // 🐺 anubis:create-normalize

    let record: TangibleThing = diesel::insert_into(tangible_things::table)
        .values(NewTangibleThing {
            // The parent comes from the route, already checked against the
            // caller's membership.
            creative_concept_id: creative_concept.id,
            name,
            description,
            // 🐺 anubis:insert-values
        })
        .returning(TangibleThing::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;
    // 🐺 anubis:create-associations

    let tangible_thing = TangibleThingView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok((
        StatusCode::CREATED,
        Json(TangibleThingBody { tangible_thing }),
    ))
}

async fn show(
    State(state): State<TangibleThingState>,
    CurrentUser(user): CurrentUser,
    Path(tangible_thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, _creative_concept, membership) =
        load(&mut connection, user.id, tangible_thing_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let tangible_thing = TangibleThingView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(TangibleThingBody { tangible_thing }))
}

async fn update(
    State(state): State<TangibleThingState>,
    CurrentUser(user): CurrentUser,
    Path(tangible_thing_id): Path<Uuid>,
    Json(body): Json<UpdateTangibleThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, creative_concept, membership) =
        load(&mut connection, user.id, tangible_thing_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::validation("Name the tangible thing.")),
        other => other.map(str::to_owned),
    };
    // A blank description clears the column, which is what the form submits
    // when the user empties the field.
    let description = optional_text(body.description.as_deref());
    // 🐺 anubis:update-normalize

    // Associations are reconciled before the columns, so a request that only
    // changes an association still takes effect.
    // 🐺 anubis:update-associations

    // A submitted parent is only ever accepted from the same team's records.
    let creative_concept_id = match body.creative_concept_id {
        Some(requested) if requested != creative_concept.id => {
            let valid =
                TangibleThing::valid_creative_concepts(&mut connection, creative_concept.team_id)
                    .await
                    .map_err(log_internal)?;
            if !valid.iter().any(|candidate| candidate.id == requested) {
                return Err(ApiError::validation(
                    "That creative concept is not available to this team.",
                ));
            }
            Some(requested)
        }
        _unchanged => None,
    };

    let changes = TangibleThingChanges {
        name,
        description,
        creative_concept_id,
        // 🐺 anubis:changeset-values
    };
    if changes.is_empty() {
        let tangible_thing = TangibleThingView::one(&mut connection, record)
            .await
            .map_err(log_internal)?;
        return Ok(Json(TangibleThingBody { tangible_thing }));
    }

    let updated: TangibleThing =
        diesel::update(tangible_things::table.filter(tangible_things::id.eq(record.id)))
            .set(changes)
            .returning(TangibleThing::as_returning())
            .get_result(&mut connection)
            .await
            .map_err(log_internal)?;

    let tangible_thing = TangibleThingView::one(&mut connection, updated)
        .await
        .map_err(log_internal)?;
    Ok(Json(TangibleThingBody { tangible_thing }))
}

async fn destroy(
    State(state): State<TangibleThingState>,
    CurrentUser(user): CurrentUser,
    Path(tangible_thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _creative_concept, membership) =
        load(&mut connection, user.id, tangible_thing_id).await?;
    require(&state.roles, &membership, Action::Destroy)?;

    diesel::delete(tangible_things::table.filter(tangible_things::id.eq(tangible_thing.id)))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Loads the parent creative concept, answering `404` when it is unreachable.
async fn load_parent(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    creative_concept_id: Uuid,
) -> Result<(CreativeConcept, TeamMembership), ApiError> {
    CreativeConcept::load_for_member(connection, user_id, creative_concept_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads the tangible thing, answering `404` when it is absent or unreachable.
async fn load(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    tangible_thing_id: Uuid,
) -> Result<(TangibleThing, CreativeConcept, TeamMembership), ApiError> {
    TangibleThing::load_for_member(connection, user_id, tangible_thing_id)
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
        "tangible thing request failed: {{error.message}}",
    );
    ApiError::internal()
}
