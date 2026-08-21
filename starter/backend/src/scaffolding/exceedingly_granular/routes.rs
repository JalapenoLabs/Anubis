//! The two surfaces of `GranularDetail`: account routes and `/api/v1`.
//!
//! Every account route resolves the whole ownership chain before it touches a
//! record: the collection routes through their parent tangible thing, which in
//! turn is loaded through its own creative concept, and the member routes
//! through the granular detail's own two parents. A crafted path is refused at
//! every hop, because each hop is a join rather than a trusted id, and a record
//! the caller cannot reach answers `404` identically to one that does not exist.
//!
//! The team is read off the chain's root, the creative concept, which is the
//! only link that carries a `team_id`. Selecting it alongside the parent is
//! what lets a third ownership level cost one more join and no denormalized
//! tenant column.
//!
//! The `/api/v1` routes are the same slice for a platform application's bearer
//! token: [`ApiCaller`] resolves the token to its team, and the chain ends at
//! that same root in one query. Both surfaces read and write through the same
//! request bodies, the same view, and the same three functions below, which is
//! what keeps the published contract and the browser's shape one thing rather
//! than two that drift.
//!
//! | Method | Account path | API path |
//! |---|---|---|
//! | GET, POST | `/account/tangible-things/{tangible_thing_id}/granular-details` | `/api/v1/tangible-things/{tangible_thing_id}/granular-details` |
//! | GET, PATCH, DELETE | `/account/granular-details/{granular_detail_id}` | `/api/v1/granular-details/{granular_detail_id}` |

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
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use super::model::{GranularDetail, GranularDetailChanges, MODEL, NewGranularDetail, SORTABLE};
use crate::scaffolding::absolutely_abstract::CreativeConcept;
use crate::scaffolding::completely_concrete::TangibleThing;
use crate::schema::granular_details;

/// The event types this model publishes to outgoing webhooks.
///
/// `<model>.<action>` in snake case, the model singular, which is the
/// convention `anubis::webhooks` documents and a team subscribes to by name.
/// The payload is the same `GranularDetailView` the endpoints below answer
/// with, so a receiver reading the published OpenAPI document already knows
/// the shape.
const CREATED_EVENT: &str = "granular_detail.created";
const UPDATED_EVENT: &str = "granular_detail.updated";
const DESTROYED_EVENT: &str = "granular_detail.destroyed";

/// Returns the granular detail routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/tangible-things/{tangible_thing_id}/granular-details",
            get(list).post(create),
        )
        .route(
            "/granular-details/{granular_detail_id}",
            get(show).patch(update).delete(destroy),
        )
        // 🐺 anubis:account-routes
        .with_state(GranularDetailState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser resolves its dependencies from these request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

/// Returns the granular detail routes of the public API.
///
/// Mounted by `anubis::api::v1::router_with`, which supplies the database
/// extension [`ApiCaller`] resolves its bearer token through.
pub fn api_router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/tangible-things/{tangible_thing_id}/granular-details",
            get(api_list).post(api_create),
        )
        .route(
            "/granular-details/{granular_detail_id}",
            get(api_show).patch(api_update).delete(api_destroy),
        )
        .with_state(GranularDetailState { pool, roles })
}

/// This model's half of the application's OpenAPI document.
///
/// The application merges it in `lib.rs`, one line per model. Only the schemas
/// this module declares are registered here: [`GranularDetail`] itself, the
/// shared error shape, and the pagination object all come from documents this
/// one is merged with.
#[derive(OpenApi)]
#[openapi(
    paths(api_list, api_show, api_create, api_update, api_destroy),
    components(schemas(
        GranularDetail,
        GranularDetailView,
        GranularDetailBody,
        GranularDetailsBody,
        CreateGranularDetailBody,
        UpdateGranularDetailBody,
    ))
)]
struct ApiDoc;

/// The OpenAPI 3.1 registrations this model contributes.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[derive(Clone)]
struct GranularDetailState {
    pool: DbPool,
    roles: RoleSet,
}

/// The list endpoint's filters, whitelisted per model by the scaffolder.
#[derive(Debug, Deserialize)]
struct GranularDetailFilters {
    /// Case-insensitive substring match on the name.
    name: Option<String>,
}

/// The fields a granular detail is created from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct CreateGranularDetailBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
    // 🐺 anubis:create-body
}

/// The fields a granular detail is updated from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct UpdateGranularDetailBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
    /// Moves the granular detail to another tangible thing of the same team.
    tangible_thing_id: Option<Uuid>,
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
#[schema(description = "A granular detail, as every endpoint serializes it.")]
struct GranularDetailView {
    #[serde(flatten)]
    granular_detail: GranularDetail,
    // 🐺 anubis:view-fields
}

impl GranularDetailView {
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
        records: Vec<GranularDetail>,
    ) -> QueryResult<Vec<Self>> {
        // 🐺 anubis:view-load
        Ok(records
            .into_iter()
            .map(|record| Self {
                // 🐺 anubis:view-values
                granular_detail: record,
            })
            .collect())
    }

    /// Builds the view for one record.
    async fn one(connection: &mut AsyncPgConnection, record: GranularDetail) -> QueryResult<Self> {
        let mut views = Self::load(connection, vec![record]).await?;
        Ok(views.pop().expect("load yields one view per record"))
    }
}

/// One granular detail, as every endpoint that answers with one wraps it.
#[derive(Serialize, ToSchema)]
struct GranularDetailBody {
    granular_detail: GranularDetailView,
}

/// A page of granular details, in the locked list envelope.
#[derive(Serialize, ToSchema)]
struct GranularDetailsBody {
    granular_details: Vec<GranularDetailView>,
    pagination: Pagination,
}

// ---------------------------------------------------------------------------
// The work, shared by both surfaces. Everything above the query is
// authorization, and that is the only thing the two surfaces do differently.
// ---------------------------------------------------------------------------

/// Reads one page of a tangible thing's granular details.
async fn list_page(
    connection: &mut AsyncPgConnection,
    tangible_thing: &TangibleThing,
    params: &ListParams,
    filters: &GranularDetailFilters,
) -> Result<GranularDetailsBody, ApiError> {
    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = granular_details::table
        .filter(granular_details::tangible_thing_id.eq(tangible_thing.id))
        .into_boxed();
    if let Some(pattern) = pattern.clone() {
        counted = counted.filter(granular_details::name.ilike(pattern));
    }
    let total_items: i64 = counted
        .count()
        .get_result(connection)
        .await
        .map_err(log_internal)?;

    let mut page = granular_details::table
        .filter(granular_details::tangible_thing_id.eq(tangible_thing.id))
        .into_boxed();
    if let Some(pattern) = pattern {
        page = page.filter(granular_details::name.ilike(pattern));
    }

    // The trailing id keeps paging stable when a sort key ties.
    let (field, descending) = params.sort(&SORTABLE, "created_at");
    page = match (field, descending) {
        ("name", false) => page.order(granular_details::name.asc()),
        ("name", true) => page.order(granular_details::name.desc()),
        ("updated_at", false) => page.order(granular_details::updated_at.asc()),
        ("updated_at", true) => page.order(granular_details::updated_at.desc()),
        (_, false) => page.order(granular_details::created_at.asc()),
        (_, true) => page.order(granular_details::created_at.desc()),
    }
    .then_order_by(granular_details::id.asc());

    let records: Vec<GranularDetail> = page
        .limit(params.limit())
        .offset(params.offset())
        .select(GranularDetail::as_select())
        .load(connection)
        .await
        .map_err(log_internal)?;
    let granular_details = GranularDetailView::load(connection, records)
        .await
        .map_err(log_internal)?;

    Ok(GranularDetailsBody {
        granular_details,
        pagination: Pagination::new(params, total_items),
    })
}

/// Writes one new granular detail under a tangible thing.
async fn insert_record(
    connection: &mut AsyncPgConnection,
    tangible_thing: &TangibleThing,
    creative_concept: &CreativeConcept,
    body: CreateGranularDetailBody,
    context: &anubis::audit::Context,
) -> Result<GranularDetailView, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("Name the granular detail."));
    }
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // A scaffolded association validates its submitted id here, so the team is
    // in scope before the anchor rather than after it. It comes off the chain's
    // root, which is the only link that knows the team.
    let team_id = creative_concept.team_id;
    // 🐺 anubis:create-normalize

    // The record, its associations, and the event they produce share one
    // transaction, so a webhook is exactly as durable as the row that caused
    // it: a rollback sends nothing, and a commit never loses its event. That
    // is what a queue in Postgres buys, and why emission takes a connection.
    connection
        .transaction::<GranularDetailView, ApiError, _>(async |connection| {
            let record: GranularDetail = diesel::insert_into(granular_details::table)
                .values(NewGranularDetail {
                    // The parent comes from the route, already checked against
                    // the caller's membership or the token's team.
                    tangible_thing_id: tangible_thing.id,
                    name,
                    description,
                    // 🐺 anubis:insert-values
                })
                .returning(GranularDetail::as_returning())
                .get_result(connection)
                .await?;
            // 🐺 anubis:create-associations

            let (record_id, label) = (record.id, record.name.clone());
            let granular_detail = GranularDetailView::one(connection, record).await?;
            anubis::webhooks::emit(connection, team_id, CREATED_EVENT, &granular_detail).await?;
            // Audited from the same seam the webhook is emitted from, so
            // every scaffolded model is in the team's log with no code of
            // its own. See `docs/audit.md`.
            anubis::audit::record(
                connection,
                context,
                &anubis::audit::Event::created(MODEL, record_id)
                    .team(team_id)
                    .label(&label),
            )
            .await?;
            Ok(granular_detail)
        })
        .await
}

/// Applies a submitted change to one granular detail.
///
/// A request that submits nothing answers with the record untouched, because
/// Diesel refuses an empty change set.
async fn apply_changes(
    connection: &mut AsyncPgConnection,
    record: GranularDetail,
    tangible_thing: &TangibleThing,
    creative_concept: &CreativeConcept,
    body: UpdateGranularDetailBody,
    context: &anubis::audit::Context,
) -> Result<GranularDetailView, ApiError> {
    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::validation("Name the granular detail.")),
        other => other.map(str::to_owned),
    };
    // A blank description clears the column, which is what the form submits
    // when the user empties the field.
    let description = optional_text(body.description.as_deref());
    // A scaffolded association validates its submitted id here, so the team is
    // in scope before the anchor rather than after it.
    let team_id = creative_concept.team_id;
    // 🐺 anubis:update-normalize

    // The record as it stood, so the audit event can say what moved. A
    // clone rather than a re-read: this is the row the handler already
    // loaded and authorized.
    let before = record.clone();

    connection
        .transaction::<GranularDetailView, ApiError, _>(async |connection| {
            // Associations are reconciled before the columns, so a request that
            // only changes an association still takes effect.
            // 🐺 anubis:update-associations

            // A submitted parent is only ever accepted from the same team's
            // records, which for a nested parent means a join rather than a
            // comparison.
            let tangible_thing_id = match body.tangible_thing_id {
                Some(requested) if requested != tangible_thing.id => {
                    let valid = GranularDetail::valid_tangible_things(connection, team_id).await?;
                    if !valid.iter().any(|candidate| candidate.id == requested) {
                        return Err(ApiError::validation(
                            "That tangible thing is not available to this team.",
                        ));
                    }
                    Some(requested)
                }
                _unchanged => None,
            };

            let changes = GranularDetailChanges {
                name,
                description,
                tangible_thing_id,
                // 🐺 anubis:changeset-values
            };
            let ((granular_detail, moved), label) = if changes.is_empty() {
                let label = record.name.clone();
                (
                    (
                        GranularDetailView::one(connection, record).await?,
                        anubis::audit::Changes::new(),
                    ),
                    label,
                )
            } else {
                let updated: GranularDetail = diesel::update(
                    granular_details::table.filter(granular_details::id.eq(record.id)),
                )
                .set(changes)
                .returning(GranularDetail::as_returning())
                .get_result(connection)
                .await?;
                // The change set is the diff of the record itself, which is
                // why a column added by `anubis scaffold field` is audited
                // the moment it exists.
                let moved = anubis::audit::Changes::between(&before, &updated)?;
                let label = updated.name.clone();
                (
                    (GranularDetailView::one(connection, updated).await?, moved),
                    label,
                )
            };

            // Emitted even when the change set was empty, because an
            // association reconciled above is a change the columns cannot see.
            anubis::webhooks::emit(connection, team_id, UPDATED_EVENT, &granular_detail).await?;
            anubis::audit::record(
                connection,
                context,
                &anubis::audit::Event::updated(MODEL, before.id)
                    .team(team_id)
                    .label(&label)
                    .changes(moved),
            )
            .await?;
            Ok(granular_detail)
        })
        .await
}

/// Deletes one granular detail, and tells the team's webhook endpoints.
///
/// The record is serialized before it is deleted, so the event carries the
/// granular detail as it last stood rather than an id and nothing else.
async fn delete_record(
    connection: &mut AsyncPgConnection,
    record: GranularDetail,
    creative_concept: &CreativeConcept,
    context: &anubis::audit::Context,
) -> Result<(), ApiError> {
    let team_id = creative_concept.team_id;
    let record_id = record.id;
    let label = record.name.clone();

    connection
        .transaction::<(), ApiError, _>(async |connection| {
            let granular_detail = GranularDetailView::one(connection, record).await?;
            diesel::delete(granular_details::table.filter(granular_details::id.eq(record_id)))
                .execute(connection)
                .await?;
            anubis::webhooks::emit(connection, team_id, DESTROYED_EVENT, &granular_detail).await?;
            anubis::audit::record(
                connection,
                context,
                &anubis::audit::Event::destroyed(MODEL, record_id)
                    .team(team_id)
                    .label(&label),
            )
            .await?;
            Ok(())
        })
        .await
}

// ---------------------------------------------------------------------------
// Account handlers: a signed-in user, authorized through their membership.
// ---------------------------------------------------------------------------

async fn list(
    State(state): State<GranularDetailState>,
    CurrentUser(user): CurrentUser,
    Path(tangible_thing_id): Path<Uuid>,
    Query(params): Query<ListParams>,
    Query(filters): Query<GranularDetailFilters>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _creative_concept, membership) =
        load_parent(&mut connection, user.id, tangible_thing_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    Ok(Json(
        list_page(&mut connection, &tangible_thing, &params, &filters).await?,
    ))
}

async fn create(
    State(state): State<GranularDetailState>,
    CurrentUser(user): CurrentUser,
    context: anubis::audit::Context,
    Path(tangible_thing_id): Path<Uuid>,
    Json(body): Json<CreateGranularDetailBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, creative_concept, membership) =
        load_parent(&mut connection, user.id, tangible_thing_id).await?;
    require(&state.roles, &membership, Action::Create)?;

    let granular_detail = insert_record(
        &mut connection,
        &tangible_thing,
        &creative_concept,
        body,
        &context.by(&user),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(GranularDetailBody { granular_detail }),
    ))
}

async fn show(
    State(state): State<GranularDetailState>,
    CurrentUser(user): CurrentUser,
    Path(granular_detail_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, _tangible_thing, _creative_concept, membership) =
        load(&mut connection, user.id, granular_detail_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let granular_detail = GranularDetailView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(GranularDetailBody { granular_detail }))
}

async fn update(
    State(state): State<GranularDetailState>,
    CurrentUser(user): CurrentUser,
    context: anubis::audit::Context,
    Path(granular_detail_id): Path<Uuid>,
    Json(body): Json<UpdateGranularDetailBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, tangible_thing, creative_concept, membership) =
        load(&mut connection, user.id, granular_detail_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    let granular_detail = apply_changes(
        &mut connection,
        record,
        &tangible_thing,
        &creative_concept,
        body,
        &context.by(&user),
    )
    .await?;
    Ok(Json(GranularDetailBody { granular_detail }))
}

async fn destroy(
    State(state): State<GranularDetailState>,
    CurrentUser(user): CurrentUser,
    context: anubis::audit::Context,
    Path(granular_detail_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (granular_detail, _tangible_thing, creative_concept, membership) =
        load(&mut connection, user.id, granular_detail_id).await?;
    require(&state.roles, &membership, Action::Destroy)?;

    delete_record(
        &mut connection,
        granular_detail,
        &creative_concept,
        &context.by(&user),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// 🐺 anubis:handlers

// ---------------------------------------------------------------------------
// API handlers: a platform application's bearer token, acting as its team.
// ---------------------------------------------------------------------------

/// List a tangible thing's granular details.
#[utoipa::path(
    get,
    path = "/api/v1/tangible-things/{tangible_thing_id}/granular-details",
    operation_id = "listGranularDetails",
    tag = "granular-details",
    params(
        ("tangible_thing_id" = Uuid, Path, description = "The parent tangible thing's id"),
        ("page" = Option<i64>, Query, description = "1-based page number, defaulting to 1"),
        ("limit" = Option<i64>, Query, description = "Page size, defaulting to 25 and capped at 100"),
        ("sort" = Option<String>, Query, description = "A sortable field, `-` prefixed for descending"),
        ("name" = Option<String>, Query, description = "Case-insensitive substring match on the name"),
    ),
    responses(
        (status = 200, description = "A page of granular details", body = GranularDetailsBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such tangible thing in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_list(
    State(state): State<GranularDetailState>,
    caller: ApiCaller,
    Path(tangible_thing_id): Path<Uuid>,
    Query(params): Query<ListParams>,
    Query(filters): Query<GranularDetailFilters>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, _creative_concept) =
        load_parent_for_token(&mut connection, &caller, tangible_thing_id).await?;

    Ok(Json(
        list_page(&mut connection, &tangible_thing, &params, &filters).await?,
    ))
}

/// Fetch one granular detail.
#[utoipa::path(
    get,
    path = "/api/v1/granular-details/{granular_detail_id}",
    operation_id = "showGranularDetail",
    tag = "granular-details",
    params(("granular_detail_id" = Uuid, Path, description = "The granular detail's id")),
    responses(
        (status = 200, description = "The granular detail", body = GranularDetailBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such granular detail in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_show(
    State(state): State<GranularDetailState>,
    caller: ApiCaller,
    Path(granular_detail_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, _tangible_thing, _creative_concept) =
        load_for_token(&mut connection, &caller, granular_detail_id).await?;

    let granular_detail = GranularDetailView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(GranularDetailBody { granular_detail }))
}

/// Create a granular detail under a tangible thing.
#[utoipa::path(
    post,
    path = "/api/v1/tangible-things/{tangible_thing_id}/granular-details",
    operation_id = "createGranularDetail",
    tag = "granular-details",
    params(("tangible_thing_id" = Uuid, Path, description = "The parent tangible thing's id")),
    request_body = CreateGranularDetailBody,
    responses(
        (status = 201, description = "The created granular detail", body = GranularDetailBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such tangible thing in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_create(
    State(state): State<GranularDetailState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Path(tangible_thing_id): Path<Uuid>,
    Json(body): Json<CreateGranularDetailBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Create, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (tangible_thing, creative_concept) =
        load_parent_for_token(&mut connection, &caller, tangible_thing_id).await?;

    let granular_detail = insert_record(
        &mut connection,
        &tangible_thing,
        &creative_concept,
        body,
        &context.by_application(&caller.application),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(GranularDetailBody { granular_detail }),
    ))
}

/// Update one granular detail.
#[utoipa::path(
    patch,
    path = "/api/v1/granular-details/{granular_detail_id}",
    operation_id = "updateGranularDetail",
    tag = "granular-details",
    params(("granular_detail_id" = Uuid, Path, description = "The granular detail's id")),
    request_body = UpdateGranularDetailBody,
    responses(
        (status = 200, description = "The updated granular detail", body = GranularDetailBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such granular detail in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_update(
    State(state): State<GranularDetailState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Path(granular_detail_id): Path<Uuid>,
    Json(body): Json<UpdateGranularDetailBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Update, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, tangible_thing, creative_concept) =
        load_for_token(&mut connection, &caller, granular_detail_id).await?;

    let granular_detail = apply_changes(
        &mut connection,
        record,
        &tangible_thing,
        &creative_concept,
        body,
        &context.by_application(&caller.application),
    )
    .await?;
    Ok(Json(GranularDetailBody { granular_detail }))
}

/// Delete one granular detail.
#[utoipa::path(
    delete,
    path = "/api/v1/granular-details/{granular_detail_id}",
    operation_id = "deleteGranularDetail",
    tag = "granular-details",
    params(("granular_detail_id" = Uuid, Path, description = "The granular detail's id")),
    responses(
        (status = 204, description = "The granular detail is gone"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such granular detail in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_destroy(
    State(state): State<GranularDetailState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Path(granular_detail_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Destroy, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (granular_detail, _tangible_thing, creative_concept) =
        load_for_token(&mut connection, &caller, granular_detail_id).await?;

    delete_record(
        &mut connection,
        granular_detail,
        &creative_concept,
        &context.by_application(&caller.application),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Loads the parent tangible thing and the chain's root, or answers `404`.
///
/// The parent's own loader walks the rest of the chain, so this hop cannot be
/// skipped by naming a tangible thing of another team: it answers `None` for
/// one, exactly as it does for an id that names nothing.
async fn load_parent(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    tangible_thing_id: Uuid,
) -> Result<(TangibleThing, CreativeConcept, TeamMembership), ApiError> {
    TangibleThing::load_for_member(connection, user_id, tangible_thing_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads the parent the token's team owns, and the chain's root, or answers `404`.
async fn load_parent_for_token(
    connection: &mut AsyncPgConnection,
    caller: &ApiCaller,
    tangible_thing_id: Uuid,
) -> Result<(TangibleThing, CreativeConcept), ApiError> {
    TangibleThing::load_for_team(connection, caller.team.id, tangible_thing_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads the granular detail, answering `404` when it is absent or unreachable.
async fn load(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    granular_detail_id: Uuid,
) -> Result<
    (
        GranularDetail,
        TangibleThing,
        CreativeConcept,
        TeamMembership,
    ),
    ApiError,
> {
    GranularDetail::load_for_member(connection, user_id, granular_detail_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Loads the granular detail the token's team owns, or answers `404`.
async fn load_for_token(
    connection: &mut AsyncPgConnection,
    caller: &ApiCaller,
    granular_detail_id: Uuid,
) -> Result<(GranularDetail, TangibleThing, CreativeConcept), ApiError> {
    GranularDetail::load_for_team(connection, caller.team.id, granular_detail_id)
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
        "granular detail request failed: {{error.message}}",
    );
    ApiError::internal()
}
