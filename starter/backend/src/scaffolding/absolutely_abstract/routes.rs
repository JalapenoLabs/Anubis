//! The two surfaces of `CreativeConcept`: account routes and `/api/v1`.
//!
//! Account collection routes hang off the team, so they authorize with the
//! framework's [`TeamMember`] guard directly. Account member routes are shallow
//! (`/creative-concepts/{id}`), so they resolve the ownership chain themselves
//! through [`CreativeConcept::load_for_member`] and authorize against the same
//! compiled [`RoleSet`]. Both paths answer `404` for records the caller cannot
//! reach, so an id probe cannot tell a missing record from another tenant's.
//!
//! The `/api/v1` routes are the same slice for a platform application's bearer
//! token: [`ApiCaller`] resolves the token to its team, which is the whole
//! ownership chain, and the token acts with that team's rights. Both surfaces
//! read and write through the same request bodies, the same view, and the same
//! three functions below, which is what keeps the published contract and the
//! browser's shape one thing rather than two that drift.
//!
//! | Method | Account path | API path |
//! |---|---|---|
//! | GET, POST | `/account/teams/{team_id}/creative-concepts` | `/api/v1/creative-concepts` |
//! | GET, PATCH, DELETE | `/account/creative-concepts/{creative_concept_id}` | `/api/v1/creative-concepts/{creative_concept_id}` |

use anubis::api::v1::{ApiCaller, ErrorV1};
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
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use super::model::{CreativeConcept, CreativeConceptChanges, MODEL, NewCreativeConcept, SORTABLE};
use crate::schema::creative_concepts;

/// The event types this model publishes to outgoing webhooks.
///
/// `<model>.<action>` in snake case, the model singular, which is the
/// convention `anubis::webhooks` documents and a team subscribes to by name.
/// The payload is the same `CreativeConceptView` the endpoints below answer
/// with, so a receiver reading the published OpenAPI document already knows
/// the shape.
const CREATED_EVENT: &str = "creative_concept.created";
const UPDATED_EVENT: &str = "creative_concept.updated";
const DESTROYED_EVENT: &str = "creative_concept.destroyed";

/// Returns the creative concept routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route("/teams/{team_id}/creative-concepts", get(list).post(create))
        .route(
            "/creative-concepts/{creative_concept_id}",
            get(show).patch(update).delete(destroy),
        )
        // 🐺 anubis:account-routes
        .with_state(CreativeConceptState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser and TeamMember resolve their dependencies from these
        // request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

/// Returns the creative concept routes of the public API.
///
/// Mounted by `anubis::api::v1::router_with`, which supplies the database
/// extension [`ApiCaller`] resolves its bearer token through.
pub fn api_router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route("/creative-concepts", get(api_list).post(api_create))
        .route(
            "/creative-concepts/{creative_concept_id}",
            get(api_show).patch(api_update).delete(api_destroy),
        )
        .with_state(CreativeConceptState { pool, roles })
}

/// This model's half of the application's OpenAPI document.
///
/// The application merges it in `lib.rs`, one line per model. Only the schemas
/// this module declares are registered here: [`CreativeConcept`] itself, the
/// shared error shape, and the pagination object all come from documents this
/// one is merged with.
#[derive(OpenApi)]
#[openapi(
    paths(api_list, api_show, api_create, api_update, api_destroy),
    components(schemas(
        CreativeConcept,
        CreativeConceptView,
        CreativeConceptBody,
        CreativeConceptsBody,
        CreateCreativeConceptBody,
        UpdateCreativeConceptBody,
    ))
)]
struct ApiDoc;

/// The OpenAPI 3.1 registrations this model contributes.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
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

/// The fields a creative concept is created from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct CreateCreativeConceptBody {
    name: String,
    /// Blank or absent stores no description.
    description: Option<String>,
    // 🐺 anubis:create-body
}

/// The fields a creative concept is updated from, on both surfaces.
#[derive(Deserialize, ToSchema)]
struct UpdateCreativeConceptBody {
    name: Option<String>,
    /// Blank clears the description, absent leaves it alone, which is exactly
    /// how the form behaves.
    description: Option<String>,
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
#[schema(description = "A creative concept, as every endpoint serializes it.")]
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

/// One creative concept, as every endpoint that answers with one wraps it.
#[derive(Serialize, ToSchema)]
struct CreativeConceptBody {
    creative_concept: CreativeConceptView,
}

/// A page of creative concepts, in the locked list envelope.
#[derive(Serialize, ToSchema)]
struct CreativeConceptsBody {
    creative_concepts: Vec<CreativeConceptView>,
    pagination: Pagination,
}

// ---------------------------------------------------------------------------
// The work, shared by both surfaces. Everything above the query is
// authorization, and that is the only thing the two surfaces do differently.
// ---------------------------------------------------------------------------

/// Reads one page of a team's creative concepts.
async fn list_page(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    params: &ListParams,
    filters: &CreativeConceptFilters,
) -> Result<CreativeConceptsBody, ApiError> {
    let pattern = like_pattern(filters.name.as_deref());

    let mut counted = creative_concepts::table
        .filter(creative_concepts::team_id.eq(team_id))
        .into_boxed();
    if let Some(pattern) = pattern.clone() {
        counted = counted.filter(creative_concepts::name.ilike(pattern));
    }
    let total_items: i64 = counted
        .count()
        .get_result(connection)
        .await
        .map_err(log_internal)?;

    let mut page = creative_concepts::table
        .filter(creative_concepts::team_id.eq(team_id))
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
        .load(connection)
        .await
        .map_err(log_internal)?;
    let creative_concepts = CreativeConceptView::load(connection, records)
        .await
        .map_err(log_internal)?;

    Ok(CreativeConceptsBody {
        creative_concepts,
        pagination: Pagination::new(params, total_items),
    })
}

/// Writes one new creative concept into a team.
async fn insert_record(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    body: CreateCreativeConceptBody,
    context: &anubis::audit::Context,
) -> Result<CreativeConceptView, ApiError> {
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

    // The record, its associations, and the event they produce share one
    // transaction, so a webhook is exactly as durable as the row that caused
    // it: a rollback sends nothing, and a commit never loses its event. That
    // is what a queue in Postgres buys, and why emission takes a connection.
    connection
        .transaction::<CreativeConceptView, ApiError, _>(async |connection| {
            let record: CreativeConcept = diesel::insert_into(creative_concepts::table)
                .values(NewCreativeConcept {
                    // The team comes from the route or the token, never from
                    // the body.
                    team_id,
                    name,
                    description,
                    // 🐺 anubis:insert-values
                })
                .returning(CreativeConcept::as_returning())
                .get_result(connection)
                .await?;
            // 🐺 anubis:create-associations

            let (record_id, label) = (record.id, record.name.clone());
            let creative_concept = CreativeConceptView::one(connection, record).await?;
            anubis::webhooks::emit(connection, team_id, CREATED_EVENT, &creative_concept).await?;
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
            Ok(creative_concept)
        })
        .await
}

/// Applies a submitted change to one creative concept.
///
/// A request that submits nothing answers with the record untouched, because
/// Diesel refuses an empty change set.
async fn apply_changes(
    connection: &mut AsyncPgConnection,
    record: CreativeConcept,
    body: UpdateCreativeConceptBody,
    context: &anubis::audit::Context,
) -> Result<CreativeConceptView, ApiError> {
    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::validation("Name the creative concept.")),
        other => other.map(str::to_owned),
    };
    // A blank description clears the column, which is what the form submits
    // when the user empties the field.
    let description = optional_text(body.description.as_deref());
    // A scaffolded association validates its submitted id here, so the team is
    // in scope before the anchor rather than after it.
    let team_id = record.team_id;
    // 🐺 anubis:update-normalize

    // The record as it stood, so the audit event can say what moved. A
    // clone rather than a re-read: this is the row the handler already
    // loaded and authorized.
    let before = record.clone();

    connection
        .transaction::<CreativeConceptView, ApiError, _>(async |connection| {
            // Associations are reconciled before the columns, so a request that
            // only changes an association still takes effect.
            // 🐺 anubis:update-associations

            let changes = CreativeConceptChanges {
                name,
                description,
                // 🐺 anubis:changeset-values
            };
            let ((creative_concept, moved), label) = if changes.is_empty() {
                let label = record.name.clone();
                (
                    (
                        CreativeConceptView::one(connection, record).await?,
                        anubis::audit::Changes::new(),
                    ),
                    label,
                )
            } else {
                let updated: CreativeConcept = diesel::update(
                    creative_concepts::table.filter(creative_concepts::id.eq(record.id)),
                )
                .set(changes)
                .returning(CreativeConcept::as_returning())
                .get_result(connection)
                .await?;
                // The change set is the diff of the record itself, which is
                // why a column added by `anubis scaffold field` is audited
                // the moment it exists.
                let moved = anubis::audit::Changes::between(&before, &updated)?;
                let label = updated.name.clone();
                (
                    (CreativeConceptView::one(connection, updated).await?, moved),
                    label,
                )
            };

            // Emitted even when the change set was empty, because an
            // association reconciled above is a change the columns cannot see.
            anubis::webhooks::emit(connection, team_id, UPDATED_EVENT, &creative_concept).await?;
            anubis::audit::record(
                connection,
                context,
                &anubis::audit::Event::updated(MODEL, before.id)
                    .team(team_id)
                    .label(&label)
                    .changes(moved),
            )
            .await?;
            Ok(creative_concept)
        })
        .await
}

/// Deletes one creative concept, and tells the team's webhook endpoints.
///
/// The record is serialized before it is deleted, so the event carries the
/// creative concept as it last stood rather than an id and nothing else.
async fn delete_record(
    connection: &mut AsyncPgConnection,
    record: CreativeConcept,
    context: &anubis::audit::Context,
) -> Result<(), ApiError> {
    let team_id = record.team_id;
    let record_id = record.id;
    let label = record.name.clone();

    connection
        .transaction::<(), ApiError, _>(async |connection| {
            let creative_concept = CreativeConceptView::one(connection, record).await?;
            diesel::delete(creative_concepts::table.filter(creative_concepts::id.eq(record_id)))
                .execute(connection)
                .await?;
            anubis::webhooks::emit(connection, team_id, DESTROYED_EVENT, &creative_concept).await?;
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
    State(state): State<CreativeConceptState>,
    member: TeamMember,
    Query(params): Query<ListParams>,
    Query(filters): Query<CreativeConceptFilters>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    Ok(Json(
        list_page(&mut connection, member.team.id, &params, &filters).await?,
    ))
}

async fn create(
    State(state): State<CreativeConceptState>,
    member: TeamMember,
    context: anubis::audit::Context,
    Json(body): Json<CreateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Create, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let creative_concept = insert_record(
        &mut connection,
        member.team.id,
        body,
        &context.by(&member.user),
    )
    .await?;
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
    context: anubis::audit::Context,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<UpdateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (record, membership) = load(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    let creative_concept = apply_changes(&mut connection, record, body, &context.by(&user)).await?;
    Ok(Json(CreativeConceptBody { creative_concept }))
}

async fn destroy(
    State(state): State<CreativeConceptState>,
    CurrentUser(user): CurrentUser,
    context: anubis::audit::Context,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Destroy)?;

    delete_record(&mut connection, creative_concept, &context.by(&user)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// 🐺 anubis:handlers

// ---------------------------------------------------------------------------
// API handlers: a platform application's bearer token, acting as its team.
// ---------------------------------------------------------------------------

/// List the team's creative concepts.
#[utoipa::path(
    get,
    path = "/api/v1/creative-concepts",
    operation_id = "listCreativeConcepts",
    tag = "creative-concepts",
    params(
        ("page" = Option<i64>, Query, description = "1-based page number, defaulting to 1"),
        ("limit" = Option<i64>, Query, description = "Page size, defaulting to 25 and capped at 100"),
        ("sort" = Option<String>, Query, description = "A sortable field, `-` prefixed for descending"),
        ("name" = Option<String>, Query, description = "Case-insensitive substring match on the name"),
    ),
    responses(
        (status = 200, description = "A page of creative concepts", body = CreativeConceptsBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_list(
    State(state): State<CreativeConceptState>,
    caller: ApiCaller,
    Query(params): Query<ListParams>,
    Query(filters): Query<CreativeConceptFilters>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    Ok(Json(
        list_page(&mut connection, caller.team.id, &params, &filters).await?,
    ))
}

/// Fetch one creative concept.
#[utoipa::path(
    get,
    path = "/api/v1/creative-concepts/{creative_concept_id}",
    operation_id = "showCreativeConcept",
    tag = "creative-concepts",
    params(("creative_concept_id" = Uuid, Path, description = "The creative concept's id")),
    responses(
        (status = 200, description = "The creative concept", body = CreativeConceptBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such creative concept in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_show(
    State(state): State<CreativeConceptState>,
    caller: ApiCaller,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let record = load_for_token(&mut connection, &caller, creative_concept_id).await?;

    let creative_concept = CreativeConceptView::one(&mut connection, record)
        .await
        .map_err(log_internal)?;
    Ok(Json(CreativeConceptBody { creative_concept }))
}

/// Create a creative concept in the token's team.
#[utoipa::path(
    post,
    path = "/api/v1/creative-concepts",
    operation_id = "createCreativeConcept",
    tag = "creative-concepts",
    request_body = CreateCreativeConceptBody,
    responses(
        (status = 201, description = "The created creative concept", body = CreativeConceptBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_create(
    State(state): State<CreativeConceptState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Json(body): Json<CreateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Create, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let creative_concept = insert_record(
        &mut connection,
        caller.team.id,
        body,
        &context.by_application(&caller.application),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreativeConceptBody { creative_concept }),
    ))
}

/// Update one creative concept.
#[utoipa::path(
    patch,
    path = "/api/v1/creative-concepts/{creative_concept_id}",
    operation_id = "updateCreativeConcept",
    tag = "creative-concepts",
    params(("creative_concept_id" = Uuid, Path, description = "The creative concept's id")),
    request_body = UpdateCreativeConceptBody,
    responses(
        (status = 200, description = "The updated creative concept", body = CreativeConceptBody),
        (status = 400, description = "The submitted fields are not valid", body = ErrorV1),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such creative concept in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_update(
    State(state): State<CreativeConceptState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<UpdateCreativeConceptBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Update, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let record = load_for_token(&mut connection, &caller, creative_concept_id).await?;

    let creative_concept = apply_changes(
        &mut connection,
        record,
        body,
        &context.by_application(&caller.application),
    )
    .await?;
    Ok(Json(CreativeConceptBody { creative_concept }))
}

/// Delete one creative concept.
#[utoipa::path(
    delete,
    path = "/api/v1/creative-concepts/{creative_concept_id}",
    operation_id = "deleteCreativeConcept",
    tag = "creative-concepts",
    params(("creative_concept_id" = Uuid, Path, description = "The creative concept's id")),
    responses(
        (status = 204, description = "The creative concept is gone"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorV1),
        (status = 403, description = "The token's roles do not grant this", body = ErrorV1),
        (status = 404, description = "No such creative concept in this team", body = ErrorV1),
    ),
    security(("bearer_token" = [])),
)]
async fn api_destroy(
    State(state): State<CreativeConceptState>,
    caller: ApiCaller,
    context: anubis::audit::Context,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(&state.roles, Action::Destroy, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let creative_concept = load_for_token(&mut connection, &caller, creative_concept_id).await?;

    delete_record(
        &mut connection,
        creative_concept,
        &context.by_application(&caller.application),
    )
    .await?;
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

/// Loads the creative concept the token's team owns, or answers `404`.
async fn load_for_token(
    connection: &mut AsyncPgConnection,
    caller: &ApiCaller,
    creative_concept_id: Uuid,
) -> Result<CreativeConcept, ApiError> {
    CreativeConcept::load_for_team(connection, caller.team.id, creative_concept_id)
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
