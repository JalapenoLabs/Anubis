//! Account (session-authenticated) endpoints for the has-many-through
//! association `IncidentalLinkage` carries.
//!
//! A join model gets no permissions of its own in `config/roles.yml`: it rides
//! the two models it links. Reading the attached records is a read on the
//! owning creative concept, changing them is an update on it, and listing the
//! options a form may offer is a read on the peripheral notions themselves.
//! One grant per real-world model is what keeps `roles.yml` legible.
//!
//! | Method | Path |
//! |---|---|
//! | GET | `/account/teams/{team_id}/incidental-linkages/options` |
//! | GET, POST | `/account/creative-concepts/{creative_concept_id}/peripheral-notions` |
//! | DELETE | `/account/creative-concepts/{creative_concept_id}/peripheral-notions/{peripheral_notion_id}` |

use anubis::auth::CurrentUser;
use anubis::db::DbPool;
use anubis::guard::TeamMember;
use anubis::http::{ApiError, FieldOption, FieldOptions};
use anubis::roles::{Action, RoleSet};
use anubis::tenancy::TeamMembership;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::{Json, Router};
use diesel_async::AsyncPgConnection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::IncidentalLinkage;
use crate::scaffolding::absolutely_abstract::{CreativeConcept, MODEL as CREATIVE_CONCEPT_MODEL};
use crate::scaffolding::merely_peripheral::{MODEL as PERIPHERAL_NOTION_MODEL, PeripheralNotion};

/// Returns the association routes, mounted under `/account`.
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route("/teams/{team_id}/incidental-linkages/options", get(options))
        .route(
            "/creative-concepts/{creative_concept_id}/peripheral-notions",
            get(list).post(attach),
        )
        .route(
            "/creative-concepts/{creative_concept_id}/peripheral-notions/{peripheral_notion_id}",
            delete(detach),
        )
        .with_state(IncidentalLinkageState {
            pool: pool.clone(),
            roles: roles.clone(),
        })
        // CurrentUser and TeamMember resolve their dependencies from these
        // request extensions.
        .layer(anubis::guard::layer(pool, roles))
}

#[derive(Clone)]
struct IncidentalLinkageState {
    pool: DbPool,
    roles: RoleSet,
}

#[derive(Serialize)]
struct AttachedPeripheralNotionsBody {
    peripheral_notions: Vec<PeripheralNotion>,
}

#[derive(Deserialize)]
struct AttachPeripheralNotionBody {
    peripheral_notion_id: Uuid,
}

/// The peripheral notions this team may attach, as select options.
async fn options(
    State(state): State<IncidentalLinkageState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    member.require(Action::Read, PERIPHERAL_NOTION_MODEL)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let valid = IncidentalLinkage::valid_peripheral_notions(&mut connection, member.team.id)
        .await
        .map_err(log_internal)?;

    Ok(Json(FieldOptions {
        options: valid
            .into_iter()
            .map(|peripheral_notion| FieldOption {
                value: peripheral_notion.id,
                label: peripheral_notion.name,
            })
            .collect(),
    }))
}

/// The peripheral notions attached to one creative concept.
async fn list(
    State(state): State<IncidentalLinkageState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load_owner(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Read)?;

    let peripheral_notions =
        IncidentalLinkage::attached_peripheral_notions(&mut connection, creative_concept.id)
            .await
            .map_err(log_internal)?;

    Ok(Json(AttachedPeripheralNotionsBody { peripheral_notions }))
}

/// Attaches one peripheral notion of the same team.
async fn attach(
    State(state): State<IncidentalLinkageState>,
    CurrentUser(user): CurrentUser,
    Path(creative_concept_id): Path<Uuid>,
    Json(body): Json<AttachPeripheralNotionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load_owner(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    IncidentalLinkage::attach(
        &mut connection,
        creative_concept.id,
        creative_concept.team_id,
        body.peripheral_notion_id,
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Detaches one peripheral notion, whether or not it was attached.
async fn detach(
    State(state): State<IncidentalLinkageState>,
    CurrentUser(user): CurrentUser,
    Path((creative_concept_id, peripheral_notion_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (creative_concept, membership) =
        load_owner(&mut connection, user.id, creative_concept_id).await?;
    require(&state.roles, &membership, Action::Update)?;

    IncidentalLinkage::detach(&mut connection, creative_concept.id, peripheral_notion_id)
        .await
        .map_err(log_internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Loads the owning creative concept, answering `404` when it is unreachable.
async fn load_owner(
    connection: &mut AsyncPgConnection,
    user_id: Uuid,
    creative_concept_id: Uuid,
) -> Result<(CreativeConcept, TeamMembership), ApiError> {
    CreativeConcept::load_for_member(connection, user_id, creative_concept_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)
}

/// Rejects with `403` unless a held role grants the action on the owning model.
fn require(roles: &RoleSet, membership: &TeamMembership, action: Action) -> Result<(), ApiError> {
    if roles.can(&membership.roles, action, CREATIVE_CONCEPT_MODEL) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You do not have permission to do that.",
        ))
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "incidental linkage request failed: {{error.message}}",
    );
    ApiError::internal()
}
