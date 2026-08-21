//! Reading the audit log: a team's, and the caller's own.
//!
//! Two routes, both read-only, because there is no other kind. An append-only
//! table with an update or delete endpoint is a table anybody can rewrite, and
//! then the log proves nothing.
//!
//! | Route | Guard | What it lists |
//! |---|---|---|
//! | `GET /teams/{team_id}/audit-events` | team admin | Everything recorded in that team |
//! | `GET /audit-events` | signed in | The caller's own account events |
//!
//! The team listing takes the admin role for the same reason the Developers
//! section does: it shows every member's activity, which is an administrative
//! view of the team rather than an editorial one. The account listing is
//! ungated beyond being signed in, because it shows a person exactly their own
//! security history and nothing else.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::AuditEvent;
use crate::auth::CurrentUser;
use crate::db::DbPool;
use crate::guard::TeamMember;
use crate::http::{ApiError, ListParams, Pagination};
use crate::roles::RoleSet;
use crate::schema::audit_events;
use crate::tenancy::ADMIN_ROLE;

/// Returns the audit log's read routes.
///
/// Mount it where the application's own account routes are mounted, so the
/// team listing sits beside the team's records:
///
/// ```ignore
/// .nest("/account", anubis::audit::router(pool.clone(), roles.clone()))
/// .nest("/account", account_router(&pool, &roles))
/// ```
pub fn router(pool: DbPool, roles: RoleSet) -> Router {
    Router::new()
        .route("/teams/{team_id}/audit-events", get(list_team_events))
        .route("/audit-events", get(list_account_events))
        .with_state(AuditState { pool: pool.clone() })
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
struct AuditState {
    pool: DbPool,
}

/// The narrowing a reader asks for, beside the page they want.
///
/// Both are exact matches rather than searches: an audit log is read by
/// following a thread ("what did this person do", "what happened to invoices"),
/// and a substring match would answer a different question less precisely.
#[derive(Deserialize)]
struct AuditFilters {
    /// Only events about this model, e.g. `CreativeConcept` or `Team`.
    subject_type: Option<String>,
    /// Only events by this user.
    actor_id: Option<Uuid>,
}

#[derive(Serialize)]
struct AuditEventsBody {
    audit_events: Vec<AuditEvent>,
    pagination: Pagination,
}

async fn list_team_events(
    State(state): State<AuditState>,
    member: TeamMember,
    Query(params): Query<ListParams>,
    Query(filters): Query<AuditFilters>,
) -> Result<impl IntoResponse, ApiError> {
    require_team_admin(&member)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    // Counted and loaded under the same filters, so the page count a reader
    // pages through matches the rows they are paging through.
    let mut counting = audit_events::table
        .filter(audit_events::team_id.eq(member.team.id))
        .into_boxed();
    let mut loading = audit_events::table
        .filter(audit_events::team_id.eq(member.team.id))
        .into_boxed();

    if let Some(subject_type) = filters.subject_type.as_deref().map(str::trim)
        && !subject_type.is_empty()
    {
        counting = counting.filter(audit_events::subject_type.eq(subject_type.to_owned()));
        loading = loading.filter(audit_events::subject_type.eq(subject_type.to_owned()));
    }
    if let Some(actor_id) = filters.actor_id {
        counting = counting.filter(audit_events::user_id.eq(actor_id));
        loading = loading.filter(audit_events::user_id.eq(actor_id));
    }

    let total_items: i64 = counting.count().get_result(&mut connection).await?;
    let events: Vec<AuditEvent> = loading
        // Newest first, which is the only order a log is read in. The id
        // breaks ties, so two events recorded in the same transaction page
        // stably rather than swapping places between requests.
        .order(audit_events::created_at.desc())
        .then_order_by(audit_events::id.asc())
        .limit(params.limit())
        .offset(params.offset())
        .select(AuditEvent::as_select())
        .load(&mut connection)
        .await?;

    Ok(axum::Json(AuditEventsBody {
        audit_events: events,
        pagination: Pagination::new(&params, total_items),
    }))
}

/// The caller's own account events: the ones that belong to no tenant.
///
/// A password change, a passkey, a second factor. They are recorded with no
/// team because they happen outside every team the account belongs to, which
/// is exactly why the team listing cannot show them and this one can.
async fn list_account_events(
    State(state): State<AuditState>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;

    let total_items: i64 = audit_events::table
        .filter(audit_events::user_id.eq(user.id))
        .filter(audit_events::team_id.is_null())
        .count()
        .get_result(&mut connection)
        .await?;

    let events: Vec<AuditEvent> = audit_events::table
        .filter(audit_events::user_id.eq(user.id))
        .filter(audit_events::team_id.is_null())
        .order(audit_events::created_at.desc())
        .then_order_by(audit_events::id.asc())
        .limit(params.limit())
        .offset(params.offset())
        .select(AuditEvent::as_select())
        .load(&mut connection)
        .await?;

    Ok(axum::Json(AuditEventsBody {
        audit_events: events,
        pagination: Pagination::new(&params, total_items),
    }))
}

/// The log shows every member's activity, so reading it is an administrative
/// act, exactly as managing platform applications and webhook endpoints is.
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
            "You need the admin role to read the team's audit log.",
        ))
    }
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "audit log request failed: {{error.message}}",
    );
    ApiError::internal()
}
