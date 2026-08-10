//! Account (session-authenticated) CRUD for `TangibleThing`.
//!
//! Every route resolves the ownership chain before it touches a record: the
//! collection routes through their parent concept, the member routes through
//! the thing's own parent. A record the caller cannot reach answers `404`,
//! identically to one that does not exist.
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
        .with_state(ThingState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser resolves its dependencies from these request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

#[derive(Clone)]
struct ThingState {
    pool: DbPool,
    roles: RoleSet,
}

/// The list endpoint's filters, whitelisted per model by the scaffolder.
#[derive(Debug, Deserialize)]
struct ThingFilters {
    /// Case-insensitive substring match on the name.
    name: Option<String>,
}

#[derive(Deserialize)]
struct CreateThingBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
}

#[derive(Deserialize)]
struct UpdateThingBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
    /// Moves the thing to another concept of the same team.
    creative_concept_id: Option<Uuid>,
}

#[derive(Serialize)]
struct ThingBody {
    tangible_thing: TangibleThing,
}

#[derive(Serialize)]
struct ThingsBody {
    tangible_things: Vec<TangibleThing>,
    pagination: Pagination,
}

async fn list(
    State(state): State<ThingState>,
    CurrentUser(user): CurrentUser,
    Path(concept_id): Path<Uuid>,
    Query(params): Query<ListParams>,
    Query(filters): Query<ThingFilters>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (concept, membership) = load_parent(&mut connection, user.id, concept_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = tangible_things::table
        .filter(tangible_things::creative_concept_id.eq(concept.id))
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
        .filter(tangible_things::creative_concept_id.eq(concept.id))
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

    Ok(Json(ThingsBody {
        tangible_things: records,
        pagination: Pagination::new(&params, total_items),
    }))
}

async fn create(
    State(state): State<ThingState>,
    CurrentUser(user): CurrentUser,
    Path(concept_id): Path<Uuid>,
    Json(body): Json<CreateThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (concept, membership) = load_parent(&mut connection, user.id, concept_id).await?;
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

    let tangible_thing: TangibleThing = diesel::insert_into(tangible_things::table)
        .values(NewTangibleThing {
            // The parent comes from the route, already checked against the
            // caller's membership.
            creative_concept_id: concept.id,
            name,
            description,
        })
        .returning(TangibleThing::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok((StatusCode::CREATED, Json(ThingBody { tangible_thing })))
}

async fn show(
    State(state): State<ThingState>,
    CurrentUser(user): CurrentUser,
    Path(thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _concept, membership) = load(&mut connection, user.id, thing_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    Ok(Json(ThingBody { tangible_thing }))
}

async fn update(
    State(state): State<ThingState>,
    CurrentUser(user): CurrentUser,
    Path(thing_id): Path<Uuid>,
    Json(body): Json<UpdateThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, concept, membership) = load(&mut connection, user.id, thing_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::validation("Name the tangible thing.")),
        other => other.map(str::to_owned),
    };
    // A blank description clears the column, which is what the form submits
    // when the user empties the field.
    let description = body.description.as_deref().map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    });

    // A submitted parent is only ever accepted from the same team's concepts.
    let creative_concept_id = match body.creative_concept_id {
        Some(requested) if requested != concept.id => {
            let valid = TangibleThing::valid_creative_concepts(&mut connection, concept.team_id)
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

    if name.is_none() && description.is_none() && creative_concept_id.is_none() {
        // Nothing was submitted; Diesel rejects an empty changeset.
        return Ok(Json(ThingBody { tangible_thing }));
    }

    let tangible_thing: TangibleThing =
        diesel::update(tangible_things::table.filter(tangible_things::id.eq(tangible_thing.id)))
            .set(TangibleThingChanges {
                name,
                description,
                creative_concept_id,
            })
            .returning(TangibleThing::as_returning())
            .get_result(&mut connection)
            .await
            .map_err(log_internal)?;

    Ok(Json(ThingBody { tangible_thing }))
}

async fn destroy(
    State(state): State<ThingState>,
    CurrentUser(user): CurrentUser,
    Path(thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _concept, membership) = load(&mut connection, user.id, thing_id).await?;
    require(&state.roles, &membership, Action::Destroy)?;

    diesel::delete(tangible_things::table.filter(tangible_things::id.eq(tangible_thing.id)))
        .execute(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Loads the parent concept, answering `404` when it is absent or out of reach.
async fn load_parent(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    concept_id: Uuid,
) -> Result<(CreativeConcept, TeamMembership), ApiError> {
    CreativeConcept::load_for_member(connection, user_id, concept_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads the thing, answering `404` when it is absent or out of reach.
async fn load(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    thing_id: Uuid,
) -> Result<(TangibleThing, CreativeConcept, TeamMembership), ApiError> {
    TangibleThing::load_for_member(connection, user_id, thing_id)
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
