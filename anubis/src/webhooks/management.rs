//! The team-facing webhook screens: subscriptions and their delivery log.
//!
//! [`router`] returns the routes an application mounts beside platform
//! applications under `/developers`. Every route is team-scoped through the
//! [`TeamMember`] guard and additionally requires the admin role, for the same
//! reason platform applications do: a subscription decides where a team's
//! records are sent, which is an administrative decision, not an editorial one.
//!
//! A signing secret leaves the server exactly once, in the create response. A
//! team that loses one deletes the endpoint and creates another; there is no
//! rotation endpoint, because rotating in place would silently break every
//! receiver until its operator noticed.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::delivery::{self, DeliverWebhook, WebhookDelivery};
use super::endpoint::{self, WebhookEndpoint, WebhookEndpointChanges};
use crate::auth::secret_box::SecretKey;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::TeamMember;
use crate::http::{ApiError, ListParams, Pagination};
use crate::jobs;
use crate::roles::RoleSet;
use crate::schema::{webhook_deliveries, webhook_endpoints};
use crate::tenancy::ADMIN_ROLE;

/// Returns the webhook subscription and debugging routes.
///
/// Mount it where platform applications are mounted:
///
/// ```ignore
/// .nest("/developers", anubis::api::management::router(pool.clone(), roles.clone()))
/// .nest("/developers", anubis::webhooks::router(pool.clone(), roles.clone(), &config))
/// ```
pub fn router(pool: DbPool, roles: RoleSet, config: &AppConfig) -> Router {
    Router::new()
        .route(
            "/teams/{team_id}/webhook-endpoints",
            get(list_endpoints).post(create_endpoint),
        )
        .route(
            "/teams/{team_id}/webhook-endpoints/{endpoint_id}",
            axum::routing::patch(update_endpoint).delete(delete_endpoint),
        )
        .route(
            "/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries",
            get(list_deliveries),
        )
        .route(
            "/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries/{delivery_id}/redeliver",
            post(redeliver),
        )
        .with_state(WebhookState {
            pool: pool.clone(),
            key: config.secret_key.clone(),
            allow_insecure: !config.environment.is_production(),
        })
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
struct WebhookState {
    pool: DbPool,
    key: SecretKey,
    /// Whether a plain `http://` endpoint may be subscribed.
    allow_insecure: bool,
}

#[derive(Deserialize)]
struct CreateEndpointBody {
    url: String,
    description: Option<String>,
    event_types: Vec<String>,
}

#[derive(Deserialize)]
struct UpdateEndpointBody {
    url: Option<String>,
    /// Blank clears the description, absent leaves it alone.
    description: Option<String>,
    event_types: Option<Vec<String>>,
    active: Option<bool>,
}

#[derive(Serialize)]
struct EndpointsBody {
    webhook_endpoints: Vec<WebhookEndpoint>,
}

#[derive(Serialize)]
struct EndpointBody {
    webhook_endpoint: WebhookEndpoint,
}

#[derive(Serialize)]
struct CreatedEndpointBody {
    webhook_endpoint: WebhookEndpoint,
    /// Shown exactly once; only its sealed form is stored.
    secret: String,
}

#[derive(Serialize)]
struct DeliveriesBody {
    webhook_deliveries: Vec<WebhookDelivery>,
    pagination: Pagination,
}

#[derive(Serialize)]
struct DeliveryBody {
    webhook_delivery: WebhookDelivery,
}

async fn list_endpoints(
    State(state): State<WebhookState>,
    member: TeamMember,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let endpoints: Vec<WebhookEndpoint> = webhook_endpoints::table
        .filter(webhook_endpoints::team_id.eq(member.team.id))
        .select(WebhookEndpoint::as_select())
        .order(webhook_endpoints::created_at.asc())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(EndpointsBody {
        webhook_endpoints: endpoints,
    }))
}

async fn create_endpoint(
    State(state): State<WebhookState>,
    member: TeamMember,
    Json(body): Json<CreateEndpointBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let url =
        endpoint::validate_url(&body.url, state.allow_insecure).map_err(ApiError::validation)?;
    let event_types = accepted_event_types(body.event_types)?;
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let (webhook_endpoint, secret) = endpoint::create(
        &mut connection,
        &state.key,
        member.team.id,
        &url,
        description,
        &event_types,
    )
    .await
    .map_err(log_internal)?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedEndpointBody {
            webhook_endpoint,
            secret,
        }),
    ))
}

async fn update_endpoint(
    State(state): State<WebhookState>,
    member: TeamMember,
    Path((_team_id, endpoint_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEndpointBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let existing = endpoint::load_for_team(&mut connection, member.team.id, endpoint_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)?;

    let url = match body.url {
        None => None,
        Some(url) => {
            Some(endpoint::validate_url(&url, state.allow_insecure).map_err(ApiError::validation)?)
        }
    };
    let event_types = match body.event_types {
        None => None,
        Some(requested) => Some(accepted_event_types(requested)?),
    };
    let description = body.description.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    });

    let webhook_endpoint = endpoint::update(
        &mut connection,
        existing,
        WebhookEndpointChanges {
            url,
            description,
            event_types,
            active: body.active,
        },
    )
    .await
    .map_err(log_internal)?;

    Ok(Json(EndpointBody { webhook_endpoint }))
}

async fn delete_endpoint(
    State(state): State<WebhookState>,
    member: TeamMember,
    Path((_team_id, endpoint_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // The deliveries cascade with the endpoint: a subscription that is gone has
    // no history worth keeping, and the team asked for it to be gone.
    let deleted = diesel::delete(
        webhook_endpoints::table
            .filter(webhook_endpoints::id.eq(endpoint_id))
            .filter(webhook_endpoints::team_id.eq(member.team.id)),
    )
    .execute(&mut connection)
    .await
    .map_err(log_internal)?;

    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_deliveries(
    State(state): State<WebhookState>,
    member: TeamMember,
    Path((_team_id, endpoint_id)): Path<(Uuid, Uuid)>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    // Resolving the endpoint through the team is what scopes the deliveries;
    // another tenant's id is a 404 before any delivery is read.
    let endpoint = endpoint::load_for_team(&mut connection, member.team.id, endpoint_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)?;

    let total_items: i64 = webhook_deliveries::table
        .filter(webhook_deliveries::webhook_endpoint_id.eq(endpoint.id))
        .count()
        .get_result(&mut connection)
        .await
        .map_err(log_internal)?;

    let deliveries: Vec<WebhookDelivery> = webhook_deliveries::table
        .filter(webhook_deliveries::webhook_endpoint_id.eq(endpoint.id))
        // Newest first, which is the only order a delivery log is read in.
        .order(webhook_deliveries::created_at.desc())
        .then_order_by(webhook_deliveries::id.asc())
        .limit(params.limit())
        .offset(params.offset())
        .select(WebhookDelivery::as_select())
        .load(&mut connection)
        .await
        .map_err(log_internal)?;

    Ok(Json(DeliveriesBody {
        webhook_deliveries: deliveries,
        pagination: Pagination::new(&params, total_items),
    }))
}

async fn redeliver(
    State(state): State<WebhookState>,
    member: TeamMember,
    Path((_team_id, endpoint_id, delivery_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let endpoint = endpoint::load_for_team(&mut connection, member.team.id, endpoint_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)?;

    let original: Option<WebhookDelivery> = webhook_deliveries::table
        .filter(webhook_deliveries::id.eq(delivery_id))
        .filter(webhook_deliveries::webhook_endpoint_id.eq(endpoint.id))
        .select(WebhookDelivery::as_select())
        .first(&mut connection)
        .await
        .optional()
        .map_err(log_internal)?;
    let Some(original) = original else {
        return Err(ApiError::not_found());
    };

    // A fresh row rather than a reset one: the first attempt's history is what
    // the team is looking at when they press the button, and overwriting it
    // would answer their question by erasing it.
    let webhook_delivery = connection
        .transaction::<WebhookDelivery, diesel::result::Error, _>(async |connection| {
            let queued: WebhookDelivery = diesel::insert_into(webhook_deliveries::table)
                .values(delivery::NewWebhookDelivery {
                    webhook_endpoint_id: endpoint.id,
                    event_type: &original.event_type,
                    payload: &original.payload,
                })
                .returning(WebhookDelivery::as_returning())
                .get_result(connection)
                .await?;

            jobs::enqueue_query(
                connection,
                &DeliverWebhook {
                    delivery_id: queued.id,
                },
            )
            .await?;

            Ok(queued)
        })
        .await
        .map_err(log_internal)?;

    Ok((StatusCode::CREATED, Json(DeliveryBody { webhook_delivery })))
}

/// Normalizes and checks a submitted event type list.
///
/// Duplicates collapse, blanks are dropped, and a type the framework cannot
/// recognize is refused by name: a subscription to `project.create` would
/// otherwise sit there receiving nothing, and nobody would find out until they
/// went looking for an event that never arrived.
fn accepted_event_types(requested: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut accepted: Vec<String> = Vec::with_capacity(requested.len());

    for candidate in requested {
        let candidate = candidate.trim().to_owned();
        if candidate.is_empty() {
            continue;
        }
        if !super::is_event_type(&candidate) {
            return Err(ApiError::validation(format!(
                "{candidate:?} is not an event type. Event types are \
                 <model>.<action>, where the action is one of {}.",
                super::LIFECYCLE_ACTIONS.join(", "),
            )));
        }
        if !accepted.contains(&candidate) {
            accepted.push(candidate);
        }
    }

    Ok(accepted)
}

/// A subscription decides where a team's records are sent, so it needs the
/// admin key, exactly as a platform application does.
fn require_team_admin(member: &TeamMember) -> Result<(), ApiError> {
    if member
        .membership
        .roles
        .iter()
        .any(|role| role == ADMIN_ROLE)
    {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need the admin role to manage webhook endpoints.",
        ))
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "webhook management request failed: {{error.message}}",
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::accepted_event_types;
    use crate::webhooks::DeliveryStatus;

    #[test]
    fn event_types_are_trimmed_deduplicated_and_ordered_as_submitted() {
        let accepted = accepted_event_types(vec![
            "  project.created  ".to_owned(),
            String::new(),
            "project.updated".to_owned(),
            "project.created".to_owned(),
        ])
        .expect("valid types must be accepted");

        assert_eq!(accepted, vec!["project.created", "project.updated"]);
    }

    #[test]
    fn an_unrecognized_event_type_is_refused_by_name() {
        let error = accepted_event_types(vec!["project.create".to_owned()])
            .expect_err("a typo must be refused");

        assert!(error.message().contains("project.create"), "{error:?}");
        assert!(error.message().contains("created"), "{error:?}");
    }

    #[test]
    fn a_delivery_starts_out_pending() {
        // The column default the migration declares, restated where the API
        // that serves it can see it drift.
        assert_eq!(DeliveryStatus::Pending.as_str(), "pending");
    }
}
