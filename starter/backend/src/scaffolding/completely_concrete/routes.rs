//! The two surfaces of `TangibleThing`: account routes and `/api/v1`.
//!
//! Every account route resolves the ownership chain before it touches a
//! record: the collection routes through their parent creative concept, the
//! member routes through the tangible thing's own parent. A record the caller
//! cannot reach answers `404`, identically to one that does not exist.
//!
//! The `/api/v1` routes are the same slice for a platform application's bearer
//! token: [`ApiCaller`] resolves the token to its team, and the chain ends at
//! the parent creative concept's team in one query. Both surfaces read and
//! write through the same request bodies, the same view, and the same three
//! functions below, which is what keeps the published contract and the
//! browser's shape one thing rather than two that drift.
//!
//! | Method | Account path | API path |
//! |---|---|---|
//! | GET, POST | `/account/creative-concepts/{creative_concept_id}/tangible-things` | `/api/v1/creative-concepts/{creative_concept_id}/tangible-things` |
//! | GET, PATCH, DELETE | `/account/tangible-things/{tangible_thing_id}` | `/api/v1/tangible-things/{tangible_thing_id}` |

use anubis::api::v1::{ApiCaller, ErrorV1};
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
use utoipa::{OpenApi, ToSchema};
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

/// Returns the tangible thing routes of the public API.
///
/// Mounted by `anubis::api::v1::router_with`, which supplies the database
/// extension [`ApiCaller`] resolves its bearer token through.
pub fn api_router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/creative-concepts/{creative_concept_id}/tangible-things",
            get(api_list).post(api_create),
        )
        .route(
            "/tangible-things/{tangible_thing_id}",
            get(api_show).patch(api_update).delete(api_destroy),
        )
        .with_state(TangibleThingState { pool, roles })
}

/// This model's half of the application's OpenAPI document.
///
/// The application merges it in `lib.rs`, one line per model. Only the schemas
/// this module declares are registered here: [`TangibleThing`] itself, the
/// shared error shape, and the pagination object all come from documents this
/// one is merged with.
#[derive(OpenApi)]
#[openapi(
    paths(api_list, api_show, api_create, api_update, api_destroy),
    components(schemas(
        TangibleThing,
        TangibleThingView,
        TangibleThingBody,
        TangibleThingsBody,
        CreateTangibleThingBody,
        UpdateTangibleThingBody,
    ))
)]
struct ApiDoc;

/// The OpenAPI 3.1 registrations this model contributes.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
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

/// The fields a tangible thing is created from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct CreateTangibleThingBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
    // 🐺 anubis:create-body
}

/// The fields a tangible thing is updated from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct UpdateTangibleThingBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
    /// Moves the tangible thing to another creative concept of the same team.
    creative_concept_id: Option<Uuid>,
    // 🐺 anubis:update-body
}

/// The record as every endpoint serializes it.
///
/// `serde(flatten)` keeps the wire shape identical to the record's own columns,
/// so a model with no associations serializes exactly as its table does. An
/// association `anubis scaffold field` adds lands its ids here, which is what
/// lets one form read and write the same shape. It is also the API serializer:
/// utoipa reads the flatten as a composition of the record's schema, so a
/// scaffolded column reaches the published document without a second
/// declaration.
#[derive(Serialize, ToSchema)]
// The doc comment above is for whoever reads this code; the description below
// is what an API consumer reads in the published document.
#[schema(description = "A tangible thing, as every endpoint serializes it.")]
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

/// One tangible thing, as every endpoint that answers with one wraps it.
#[derive(Serialize, ToSchema)]
struct TangibleThingBody {
    tangible_thing: TangibleThingView,
}

/// A page of tangible things, in the locked list envelope.
#[derive(Serialize, ToSchema)]
struct TangibleThingsBody {
    tangible_things: Vec<TangibleThingView>,
    pagination: Pagination,
}

// ---------------------------------------------------------------------------
// The work, shared by both surfaces. Everything above the query is
// authorization, and that is the only thing the two surfaces do differently.
// ---------------------------------------------------------------------------

/// Reads one page of a creative concept's tangible things.
async fn list_page(
    connection: &mut AsyncPgConnection,
    creative_concept: &CreativeConcept,
    params: &ListParams,
    filters: &TangibleThingFilters,
) -> Result<TangibleThingsBody, ApiError> {
    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = tangible_things::table
        .filter(tangible_things::creative_concept_id.eq(creative_concept.id))
        .into_boxed();
    if let Some(pattern) = pattern.clone() {
        counted = counted.filter(tangible_things::name.ilike(pattern));
    }
    let total_items: i64 = counted
        .count()
        .get_result(connection)
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
        .load(connection)
        .await
        .map_err(log_internal)?;
    let tangible_things = TangibleThingView::load(connection, records)
        .await
        .map_err(log_internal)?;

    Ok(TangibleThingsBody {
        tangible_things,
        pagination: Pagination::new(params, total_items),
    })
}

/// Writes one new tangible thing under a creative concept.
async fn insert_record(
    connection: &mut AsyncPgConnection,
    creative_concept: &CreativeConcept,
    body: CreateTangibleThingBody,
) -> Result<TangibleThingView, ApiError> {
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
            // caller's membership or the token's team.
            creative_concept_id: creative_concept.id,
            name,
            description,
            // 🐺 anubis:insert-values
        })
        .returning(TangibleThing::as_returning())
        .get_result(connection)
        .await
        .map_err(log_internal)?;
    // 🐺 anubis:create-associations

    TangibleThingView::one(connection, record)
        .await
        .map_err(log_internal)
}

/// Applies a submitted change to one tangible thing.
///
/// A request that submits nothing answers with the record untouched, because
/// Diesel refuses an empty change set.
async fn apply_changes(
    connection: &mut AsyncPgConnection,
    record: TangibleThing,
    creative_concept: &CreativeConcept,
    body: UpdateTangibleThingBody,
) -> Result<TangibleThingView, ApiError> {
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
                TangibleThing::valid_creative_concepts(connection, creative_concept.team_id)
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
        return TangibleThingView::one(connection, record)
            .await
            .map_err(log_internal);
    }

    let updated: TangibleThing =
        diesel::update(tangible_things::table.filter(tangible_things::id.eq(record.id)))
            .set(changes)
            .returning(TangibleThing::as_returning())
            .get_result(connection)
            .await
            .map_err(log_internal)?;

    TangibleThingView::one(connection, updated)
        .await
        .map_err(log_internal)
}

// ---------------------------------------------------------------------------
// Account handlers: a signed-in user, authorized through their membership.
// ---------------------------------------------------------------------------

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

    Ok(Json(
        list_page(&mut connection, &creative_concept, &params, &filters).await?,
    ))
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

    let tangible_thing = insert_record(&mut connection, &creative_concept, body).await?;
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

    let tangible_thing = apply_changes(&mut connection, record, &creative_concept, body).await?;
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

// ---------------------------------------------------------------------------
// API handlers: a platform application's bearer token, acting as its team.
// ---------------------------------------------------------------------------

/// List a creative concept's tangible things.
#[utoipa::path(
    get,
    path = "/api/v1/creative-concepts/{creative_concept_id}/tangible-things",
    operation_id = "listTangibleThings",
    tag = "tangible-things",
    params(
        ("creative_concept_id" = Uuid, Path, description = "The parent creative concept's id"),
        ("page" = Option<i64>, Query, description = "1-based page number, defaulting to 1"),
        ("limit" = Option<i64>, Query, description = "Page size, defaulting to 25 and capped at 100"),
        ("sort" = Option<String>, Query, description = "A sortable field, `-` prefixed for descending"),
        ("name" = Option<String>, Query, description = "Case-insensitive substring match on the name"),
    ),
    responses(
        (status = 200, description = "A page of tangible things", body = TangibleThingsBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such creative concept in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_list(
    State(state): State<TangibleThingState>,
    caller: ApiCaller,
    Path(creative_concept_id): Path<Uuid>,
    Query(params): Query<ListParams>,
    Query(filters): Query<TangibleThingFilters>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let creative_concept =
        load_parent_for_token(&mut connection, &caller, creative_concept_id).await?;

    Ok(Json(
        list_page(&mut connection, &creative_concept, &params, &filters).await?,
    ))
}

/// Fetch one tangible thing.
#[utoipa::path(
    get,
    path = "/api/v1/tangible-things/{tangible_thing_id}",
    operation_id = "showTangibleThing",
    tag = "tangible-things",
    params(("tangible_thing_id" = Uuid, Path, description = "The tangible thing's id")),
    responses(
        (status = 200, description = "The tangible thing", body = TangibleThingBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such tangible thing in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_show(
    State(state): State<TangibleThingState>,
    caller: ApiCaller,
    Path(tangible_thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, _creative_concept) =
        load_for_token(&mut connection, &caller, tangible_thing_id).await?;

    let tangible_thing = TangibleThingView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(TangibleThingBody { tangible_thing }))
}

/// Create a tangible thing under a creative concept.
#[utoipa::path(
    post,
    path = "/api/v1/creative-concepts/{creative_concept_id}/tangible-things",
    operation_id = "createTangibleThing",
    tag = "tangible-things",
    params(("creative_concept_id" = Uuid, Path, description = "The parent creative concept's id")),
    request_body = CreateTangibleThingBody,
    responses(
        (status = 201, description = "The created tangible thing", body = TangibleThingBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such creative concept in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_create(
    State(state): State<TangibleThingState>,
    caller: ApiCaller,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<CreateTangibleThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Create, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let creative_concept =
        load_parent_for_token(&mut connection, &caller, creative_concept_id).await?;

    let tangible_thing = insert_record(&mut connection, &creative_concept, body).await?;
    Ok((
        StatusCode::CREATED,
        Json(TangibleThingBody { tangible_thing }),
    ))
}

/// Update one tangible thing.
#[utoipa::path(
    patch,
    path = "/api/v1/tangible-things/{tangible_thing_id}",
    operation_id = "updateTangibleThing",
    tag = "tangible-things",
    params(("tangible_thing_id" = Uuid, Path, description = "The tangible thing's id")),
    request_body = UpdateTangibleThingBody,
    responses(
        (status = 200, description = "The updated tangible thing", body = TangibleThingBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such tangible thing in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_update(
    State(state): State<TangibleThingState>,
    caller: ApiCaller,
    Path(tangible_thing_id): Path<Uuid>,
    Json(body): Json<UpdateTangibleThingBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Update, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, creative_concept) =
        load_for_token(&mut connection, &caller, tangible_thing_id).await?;

    let tangible_thing = apply_changes(&mut connection, record, &creative_concept, body).await?;
    Ok(Json(TangibleThingBody { tangible_thing }))
}

/// Delete one tangible thing.
#[utoipa::path(
    delete,
    path = "/api/v1/tangible-things/{tangible_thing_id}",
    operation_id = "deleteTangibleThing",
    tag = "tangible-things",
    params(("tangible_thing_id" = Uuid, Path, description = "The tangible thing's id")),
    responses(
        (status = 204, description = "The tangible thing is gone"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such tangible thing in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_destroy(
    State(state): State<TangibleThingState>,
    caller: ApiCaller,
    Path(tangible_thing_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Destroy, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _creative_concept) =
        load_for_token(&mut connection, &caller, tangible_thing_id).await?;

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

/// Loads the parent creative concept the token's team owns, or answers `404`.
async fn load_parent_for_token(
    connection: &mut AsyncPgConnection,
    caller: &ApiCaller,
    creative_concept_id: Uuid,
) -> Result<CreativeConcept, ApiError> {
    CreativeConcept::load_for_team(connection, caller.team.id, creative_concept_id)
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

/// Loads the tangible thing the token's team owns, or answers `404`.
async fn load_for_token(
    connection: &mut AsyncPgConnection,
    caller: &ApiCaller,
    tangible_thing_id: Uuid,
) -> Result<(TangibleThing, CreativeConcept), ApiError> {
    TangibleThing::load_for_team(connection, caller.team.id, tangible_thing_id)
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
