//! The recipient's own endpoints: read the inbox, and mark it read.
//!
//! [`router`] returns the routes an application mounts under `/account`,
//! beside its own account surface. Every route is the signed-in user's own:
//! there is no `{user_id}` to guard and no `roles.yml` entry to grant, because
//! a notification is addressed to a person rather than owned by a tenant. The
//! [`CurrentUser`] extractor is the whole of the authorization, and a
//! notification id that belongs to somebody else answers `404` exactly as one
//! that does not exist.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::model::{self, Notification};
use crate::auth::CurrentUser;
use crate::db::DbPool;
use crate::http::{ApiError, ListParams, Pagination};

/// Returns the notification routes.
///
/// Mount them where the application mounts its own account routes:
///
/// ```ignore
/// .nest("/account", anubis::notifications::router(pool.clone()))
/// .nest("/account", account_router(&pool, &roles))
/// ```
pub fn router(pool: DbPool) -> Router {
    Router::new()
        .route("/notifications", get(list))
        .route("/notifications/read-all", post(read_all))
        .route("/notifications/{notification_id}/read", post(read_one))
        .with_state(pool.clone())
        // CurrentUser resolves its pool from request extensions, so the routes
        // authenticate whether or not the application layered one on top.
        .layer(Extension(pool))
}

#[derive(Serialize)]
struct NotificationsBody {
    notifications: Vec<Notification>,
    pagination: Pagination,
    /// How many of the recipient's notifications are unread, across every
    /// page. It is what the badge shows, so it never describes one page.
    unread: i64,
}

#[derive(Serialize)]
struct NotificationBody {
    notification: Notification,
}

#[derive(Serialize)]
struct MarkedAllBody {
    /// How many notifications this request marked read.
    marked: i64,
}

/// One page of the caller's inbox, unread first and newest first.
async fn list(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = pool.get().await.map_err(log_internal)?;

    let notifications = model::page(&mut connection, user.id, &params)
        .await
        .map_err(log_internal)?;
    let (total_items, unread) = model::counts(&mut connection, user.id)
        .await
        .map_err(log_internal)?;

    Ok(Json(NotificationsBody {
        notifications,
        pagination: Pagination::new(&params, total_items),
        unread,
    }))
}

/// Marks one notification read, answering with it as it now stands.
async fn read_one(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(notification_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = pool.get().await.map_err(log_internal)?;

    let notification = model::mark_read(&mut connection, user.id, notification_id)
        .await
        .map_err(log_internal)?
        .ok_or_else(ApiError::not_found)?;

    Ok(Json(NotificationBody { notification }))
}

/// Marks everything unread read, answering with how many that was.
async fn read_all(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = pool.get().await.map_err(log_internal)?;

    let marked = model::mark_all_read(&mut connection, user.id)
        .await
        .map_err(log_internal)?;

    Ok(Json(MarkedAllBody {
        // The count is a row count, which cannot be negative and cannot exceed
        // what one person's inbox holds.
        marked: i64::try_from(marked).unwrap_or(i64::MAX),
    }))
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "notification request failed: {{error.message}}",
    );
    ApiError::internal()
}
