//! The organization-scoped billing endpoints.
//!
//! Four routes, mounted by an application under `/billing`:
//!
//! | Route | Guard | Effect |
//! |---|---|---|
//! | `GET /organizations/{organization_id}` | org member | The plan in force, every live subscription with its items, and the seats in use |
//! | `POST /organizations/{organization_id}/checkout` | org admin or billing | Opens a Stripe Checkout session and answers with its URL |
//! | `POST /organizations/{organization_id}/portal` | org admin or billing | Opens the Stripe customer portal and answers with its URL |
//! | `POST /organizations/{organization_id}/reconcile` | org admin or billing | Reads Stripe and corrects the subscriptions this application shows |
//!
//! Reading is open to every member because the plan and its limits explain
//! what the whole organization can do. Spending money is not: it takes the
//! `admin` role, or the `billing` role, which the starter's `roles.yml`
//! defines for exactly this.
//!
//! The write routes answer `503` when `STRIPE_SECRET_KEY` is unset, and the
//! read route keeps working: an application with no Stripe account is on the
//! free plan, which is a complete and correct answer.
//!
//! Subject subscriptions are listed here but bought and changed through the
//! application's own routes, which call [`super::Subscriptions`]: only the
//! application knows whether a record belongs to the organization, who may
//! spend money on it, and which prices it sells it at.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use diesel_async::AsyncPgConnection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lifecycle::{self, Reconciler};
use super::limits;
use super::model::{Subscription, SubscriptionWithItems};
use super::plans::{Interval, Plan, PlanSet};
use super::stripe::{self, LineItem, NewPortalSession, Purchase};
use super::subscriptions::{self, Subscriptions};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::OrganizationMember;
use crate::http::ApiError;
use crate::roles::RoleSet;
use crate::tenancy::ADMIN_ROLE;

/// Role key that may spend the organization's money without administering it.
///
/// The starter's `config/roles.yml` ships it, described as "can manage billing
/// and subscriptions", and this is the surface that sentence refers to.
pub const BILLING_ROLE: &str = "billing";

/// Where Stripe returns the browser, under `APP_URL`.
///
/// The frontend owns this route; the framework only has to name it, because
/// Stripe needs an absolute URL before the purchase begins. It is scoped to
/// the organization for two reasons: the page it lands on is that
/// organization's billing screen, and `/billing` itself is a reserved API
/// prefix (see [`crate::spa::RESERVED_PREFIXES`]), so a client route there
/// would answer JSON on a cold load.
fn return_url(app_url: &str, organization_id: Uuid) -> String {
    format!("{app_url}/organizations/{organization_id}/billing")
}

/// Seats bought by a checkout of a price that is not sold per seat.
///
/// One organization, one plan line. A price marked `per_seat` in
/// `billing.yml` is bought with the organization's seat count instead, and
/// [`super::SyncSeats`] keeps that number current afterwards.
const FLAT_QUANTITY: i64 = 1;

/// Returns the billing routes for an application to mount under `/billing`.
///
/// The plan set is the application's validated `config/billing.yml`; the
/// config supplies the Stripe credentials and the public base URL the return
/// links are built from.
pub fn router(pool: DbPool, roles: RoleSet, plans: PlanSet, config: &AppConfig) -> Router {
    // The service holds the same plans, because it resolves the plan a Stripe
    // price belongs to; building it here rather than taking one is what keeps
    // the application's mount a single call.
    let subscriptions = Subscriptions::new(pool.clone(), plans.clone(), config);
    let state = BillingState {
        pool: pool.clone(),
        reconciler: subscriptions.reconciler.clone(),
        stripe: subscriptions.stripe.clone(),
        subscriptions,
        plans: Arc::new(plans),
        app_url: config.app_url.clone(),
    };

    Router::new()
        .route("/organizations/{organization_id}", get(show))
        .route(
            "/organizations/{organization_id}/checkout",
            post(start_checkout),
        )
        .route("/organizations/{organization_id}/portal", post(open_portal))
        .route(
            "/organizations/{organization_id}/reconcile",
            post(reconcile),
        )
        .with_state(state)
        // The OrganizationMember guard resolves its dependencies from these.
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
struct BillingState {
    pool: DbPool,
    plans: Arc<PlanSet>,
    /// Absent until `STRIPE_SECRET_KEY` is set, which disables every write.
    stripe: Option<stripe::Client>,
    reconciler: Reconciler,
    subscriptions: Subscriptions,
    app_url: String,
}

#[derive(Serialize)]
struct BillingBody {
    /// The plan in force: the plan subscription's, or the free plan.
    plan: Plan,
    /// The plan subscription behind that plan, absent on the free plan.
    subscription: Option<SubscriptionWithItems>,
    /// Every live subscription the organization holds, each with its line
    /// items: the plan subscription first, then those paying for its records.
    subscriptions: Vec<SubscriptionWithItems>,
    /// Whether this deployment can start a checkout at all.
    billing_enabled: bool,
    /// People who can reach the organization, claimed and invited alike.
    ///
    /// The one usage figure the framework can count for every application,
    /// because memberships are its own table. A screen reads it against the
    /// plan's `seats` limit to show how full the organization is; an
    /// application's own limits are counted by the application.
    seats_used: i64,
}

#[derive(Deserialize)]
struct CheckoutBody {
    plan_key: String,
    interval: String,
}

#[derive(Serialize)]
struct RedirectBody {
    /// Where the browser has to go next, at Stripe.
    url: String,
}

/// The plan an organization is on, and every subscription it holds.
///
/// Resolution is total: a plan subscription that grants access names its plan,
/// and everything else, including no subscription at all, is the free plan.
async fn show(
    State(state): State<BillingState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let live = Subscription::live_for_organization(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?;

    let body = overview(&mut connection, &state, member.organization.id, live).await?;
    Ok(Json(body))
}

/// The body both [`show`] and [`reconcile`] answer with.
async fn overview(
    connection: &mut AsyncPgConnection,
    state: &BillingState,
    organization_id: Uuid,
    live: Vec<Subscription>,
) -> Result<BillingBody, ApiError> {
    let subscriptions = SubscriptionWithItems::load(connection, live)
        .await
        .map_err(log_internal)?;
    let seats_used = limits::seats_used(connection, organization_id)
        .await
        .map_err(log_internal)?;

    let subscription = subscriptions
        .iter()
        .find(|held| held.subscription.subject_type.is_none())
        .cloned();
    // Cloned into the response: a plan is a handful of strings, and the
    // alternative is a body that borrows the request's own state.
    let plan = limits::plan_in_force(
        &state.plans,
        subscription.as_ref().map(|held| &held.subscription),
    )
    .clone();

    Ok(BillingBody {
        plan,
        subscription,
        subscriptions,
        billing_enabled: state.stripe.is_some(),
        seats_used,
    })
}

/// Opens a Stripe Checkout session for one plan and interval.
///
/// Creates the organization's Stripe customer on the way through, if this is
/// its first purchase. The subscription itself is not written here: it exists
/// at Stripe the moment the customer pays, and arrives as an event.
async fn start_checkout(
    State(state): State<BillingState>,
    member: OrganizationMember,
    Json(body): Json<CheckoutBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    configured(&state)?;

    let plan = state.plans.find(&body.plan_key).ok_or_else(|| {
        ApiError::validation(format!(
            "No plan named {:?}. Ask for one of: {}.",
            body.plan_key,
            plan_keys(&state.plans),
        ))
    })?;
    if plan.is_free() {
        return Err(ApiError::validation(
            "The free plan needs no checkout. Cancel the current subscription instead.",
        ));
    }
    let interval = Interval::parse(&body.interval)
        .ok_or_else(|| ApiError::validation("Ask for an interval of \"monthly\" or \"yearly\"."))?;
    let price = plan.price(interval).ok_or_else(|| {
        ApiError::validation(format!("The {} plan is not sold {interval}.", plan.name()))
    })?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;

    // Changing plans is the portal's job: it prorates, it handles the card, and
    // it is the screen Stripe keeps current. Starting a second plan
    // subscription here would bill the organization twice. Subscriptions
    // paying for the organization's records are no obstacle: they buy those.
    if Subscription::current_for_organization(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "This organization already has a subscription. Change it in the billing portal.",
        ));
    }

    // A per-seat price bills for the people the organization already has, so
    // the first invoice is right without waiting for a membership to change.
    // Never zero: Stripe refuses a line item of none, and the buyer is one.
    let quantity = if price.is_per_seat() {
        limits::seats_used(&mut connection, member.organization.id)
            .await
            .map_err(log_internal)?
            .max(1)
    } else {
        FLAT_QUANTITY
    };

    drop(connection);

    let landing = return_url(&state.app_url, member.organization.id);
    let session = state
        .subscriptions
        .open_checkout(
            member.organization.id,
            Purchase::Plan(plan.key()),
            &[LineItem::new(price.stripe_price_id(), quantity)],
            &landing,
            Some(&member.user.email),
        )
        .await?;

    tracing::info!(
        organization.id = %member.organization.id,
        billing.plan = plan.key(),
        billing.interval = interval.as_str(),
        billing.quantity = quantity,
        billing.checkout.session = %session.id,
        "checkout {{billing.checkout.session}} opened for {{billing.plan}} \
         ({{billing.interval}}), {{billing.quantity}} seats",
    );

    Ok(Json(RedirectBody { url: session.url }))
}

/// Reads Stripe and corrects what this application shows.
///
/// The lifecycle keeps itself current from Stripe's events, so this exists for
/// the times it could not: an endpoint that was misconfigured for a week, a
/// deployment that was down while Stripe gave up retrying, a plan whose price
/// reached `config/billing.yml` after the first customer bought it. It answers
/// with the same body as [`show`], so the screen that triggered it re-renders
/// from the answer.
async fn reconcile(
    State(state): State<BillingState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    configured(&state)?;

    let live = state
        .reconciler
        .reconcile(member.organization.id)
        .await
        .map_err(|error| reconcile_failed(&error))?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let body = overview(&mut connection, &state, member.organization.id, live).await?;
    Ok(Json(body))
}

/// Opens the Stripe customer portal for an organization that has a customer.
async fn open_portal(
    State(state): State<BillingState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    let stripe = configured(&state)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let customer_id = subscriptions::stored_customer_id(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?
        .ok_or_else(|| {
            ApiError::conflict(
                "This organization has no billing account yet. Start a subscription first.",
            )
        })?;

    let landing = return_url(&state.app_url, member.organization.id);
    let session = stripe
        .create_portal_session(NewPortalSession {
            customer_id: &customer_id,
            return_url: &landing,
        })
        .await
        .map_err(|error| stripe_failed(&error, "open the billing portal"))?;

    Ok(Json(RedirectBody { url: session.url }))
}

/// The Stripe client, or the honest answer that this deployment has none.
fn configured(state: &BillingState) -> Result<&stripe::Client, ApiError> {
    state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "Billing is not configured for this deployment. Set STRIPE_SECRET_KEY to enable it.",
        )
    })
}

/// Refuses a member who may see the plan but not buy one.
fn require_billing_authority(member: &OrganizationMember) -> Result<(), ApiError> {
    let permitted = member
        .membership
        .roles
        .iter()
        .any(|role| role == ADMIN_ROLE || role == BILLING_ROLE);

    if permitted {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need the admin or billing role to manage this organization's subscription.",
        ))
    }
}

/// Renders a Stripe failure as an answer the caller can act on.
///
/// A refusal is a misconfiguration on this side (a price id that no longer
/// exists, an account that cannot take payments), so it reads as an internal
/// error with the detail in the log. Being unable to reach Stripe is temporary
/// and says so, because retrying is exactly what the caller should do.
fn stripe_failed(error: &stripe::Error, attempt: &str) -> ApiError {
    tracing::error!(
        error.message = %error,
        billing.attempt = attempt,
        "failed to {{billing.attempt}} at Stripe: {{error.message}}",
    );

    if error.is_transport() {
        ApiError::unavailable("Billing is temporarily unavailable. Try again in a moment.")
    } else {
        ApiError::internal()
    }
}

/// Renders a failed reconciliation as an answer the caller can act on.
///
/// The split mirrors [`stripe_failed`]: being unable to reach Stripe is
/// temporary and says so, and everything else is a misconfiguration on this
/// side with the detail in the log.
fn reconcile_failed(error: &lifecycle::Error) -> ApiError {
    tracing::error!(
        error.message = error.message(),
        "failed to reconcile a subscription with Stripe: {{error.message}}",
    );

    if error.is_transport() {
        ApiError::unavailable("Billing is temporarily unavailable. Try again in a moment.")
    } else {
        ApiError::internal()
    }
}

/// The plan keys, for an error that has to name the choices.
fn plan_keys(plans: &PlanSet) -> String {
    plans
        .plans()
        .iter()
        .map(Plan::key)
        .collect::<Vec<_>>()
        .join(", ")
}

fn log_internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(
        error.message = %error,
        "billing request failed: {{error.message}}",
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::return_url;

    #[test]
    fn the_return_url_lands_on_the_organizations_own_billing_page() {
        let organization_id = uuid::Uuid::nil();

        let landing = return_url("https://app.example.com", organization_id);

        assert_eq!(
            landing,
            "https://app.example.com/organizations/00000000-0000-0000-0000-000000000000/billing",
        );
        // Never under a prefix the API answers JSON on, or a cold load of the
        // page Stripe returns to would be a 404 the customer cannot read.
        for reserved in crate::spa::RESERVED_PREFIXES {
            assert!(
                !landing.starts_with(&format!("https://app.example.com{reserved}")),
                "the landing page must not sit under the reserved prefix {reserved}",
            );
        }
    }
}
