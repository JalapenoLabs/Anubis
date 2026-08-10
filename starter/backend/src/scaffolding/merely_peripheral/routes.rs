//! Account (session-authenticated) endpoints for `PeripheralNotion`.
//!
//! Only the two endpoints the join template's narrative needs: list the team's
//! notions, and create one. `anubis scaffold join` generates no model of its
//! own, so the far side of a join is always a model an earlier `anubis
//! scaffold model` run produced with the full set of handlers. This one stays
//! small on purpose.
//!
//! | Method | Path |
//! |---|---|
//! | GET, POST | `/account/teams/{team_id}/peripheral-notions` |

use anubis::db::DbPool;
use anubis::guard::TeamMember;
use anubis::http::{ApiError, ListParams, Pagination};
use anubis::roles::{Action, RoleSet};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};

use super::model::{MODEL, NewPeripheralNotion, PeripheralNotion};
use crate::schema::peripheral_notions;

/// Returns the peripheral notion routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route(
            "/teams/{team_id}/peripheral-notions",
            get(list).post(create),
        )
        .with_state(PeripheralNotionState { pool: pool.clone() })
        // TeamMember resolves its dependencies from these request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

#[derive(Clone)]
struct PeripheralNotionState {
    pool: DbPool,
}

#[derive(Deserialize)]
struct CreatePeripheralNotionBody {
    name: String,
}

#[derive(Serialize)]
struct PeripheralNotionBody {
    peripheral_notion: PeripheralNotion,
}

#[derive(Serialize)]
struct PeripheralNotionsBody {
    peripheral_notions: Vec<PeripheralNotion>,
    pagination: Pagination,
}

async fn list(
    State(state): State<PeripheralNotionState>,
    member: TeamMember,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Read, MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let total_items: i64 = peripheral_notions::table
        .filter(peripheral_notions::team_id.eq(member.team.id))
        .count()
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    let records: Vec<PeripheralNotion> = peripheral_notions::table
        .filter(peripheral_notions::team_id.eq(member.team.id))
        .order(peripheral_notions::name.asc())
        .limit(params.limit())
        .offset(params.offset())
        .select(PeripheralNotion::as_select())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(PeripheralNotionsBody {
        peripheral_notions: records,
        pagination: Pagination::new(&params, total_items),
    }))
}

async fn create(
    State(state): State<PeripheralNotionState>,
    member: TeamMember,
    Json(body): Json<CreatePeripheralNotionBody>,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Create, MODEL)?;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("Name the peripheral notion."));
    }

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let peripheral_notion: PeripheralNotion = diesel::insert_into(peripheral_notions::table)
        .values(NewPeripheralNotion {
            // The team comes from the route, never from the body.
            team_id: member.team.id,
            name,
        })
        .returning(PeripheralNotion::as_returning())
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok((
        StatusCode::CREATED,
        Json(PeripheralNotionBody { peripheral_notion }),
    ))
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "peripheral notion request failed: {{error.message}}",
    );
    ApiError::internal()
}
