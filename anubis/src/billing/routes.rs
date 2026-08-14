//! The organization-scoped billing endpoints.
//!
//! Three routes, mounted by an application under `/billing`:
//!
//! | Route | Guard | Effect |
//! |---|---|---|
//! | `GET /organizations/{organization_id}` | org member | The plan in force, and the subscription behind it |
//! | `POST /organizations/{organization_id}/checkout` | org admin or billing | Opens a Stripe Checkout session and answers with its URL |
//! | `POST /organizations/{organization_id}/portal` | org admin or billing | Opens the Stripe customer portal and answers with its URL |
//!
//! Reading is open to every member because the plan and its limits explain
//! what the whole organization can do. Spending money is not: it takes the
//! `admin` role, or the `billing` role, which the starter's `roles.yml`
//! defines for exactly this.
//!
//! Both write routes answer `503` when `STRIPE_SECRET_KEY` is unset, and the
//! read route keeps working: an application with no Stripe account is on the
//! free plan, which is a complete and correct answer.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::Subscription;
use super::plans::{Interval, Plan, PlanSet};
use super::stripe::{self, NewCheckoutSession, NewCustomer, NewPortalSession};
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::OrganizationMember;
use crate::http::ApiError;
use crate::roles::RoleSet;
use crate::schema::organizations;
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

/// Seats bought by a checkout the framework starts.
///
/// Flat pricing until per-seat billing lands: the column and Stripe's line
/// item both carry a quantity already, so the change is the number that goes
/// here and the event that keeps it current.
const DEFAULT_QUANTITY: i64 = 1;

/// Returns the billing routes for an application to mount under `/billing`.
///
/// The plan set is the application's validated `config/billing.yml`; the
/// config supplies the Stripe credentials and the public base URL the return
/// links are built from.
pub fn router(pool: DbPool, roles: RoleSet, plans: PlanSet, config: &AppConfig) -> Router {
    let state = BillingState {
        pool: pool.clone(),
        plans: Arc::new(plans),
        stripe: config.stripe.as_ref().map(stripe::Client::new),
        app_url: config.app_url.clone(),
    };

    Router::new()
        .route("/organizations/{organization_id}", get(show))
        .route(
            "/organizations/{organization_id}/checkout",
            post(start_checkout),
        )
        .route("/organizations/{organization_id}/portal", post(open_portal))
        .with_state(state)
        // The OrganizationMember guard resolves its dependencies from these.
        .layer(crate::guard::layer(pool, roles))
}

#[derive(Clone)]
struct BillingState {
    pool: DbPool,
    plans: Arc<PlanSet>,
    /// Absent until `STRIPE_SECRET_KEY` is set, which disables the two writes.
    stripe: Option<stripe::Client>,
    app_url: String,
}

#[derive(Serialize)]
struct BillingBody {
    /// The plan in force: the subscription's, or the free plan.
    plan: Plan,
    /// The subscription behind that plan, absent on the free plan.
    subscription: Option<Subscription>,
    /// Whether this deployment can start a checkout at all.
    billing_enabled: bool,
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

/// The plan an organization is on, and the subscription behind it.
///
/// Resolution is total: a subscription that grants access names its plan, and
/// everything else, including no subscription at all, is the free plan.
async fn show(
    State(state): State<BillingState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let subscription =
        Subscription::current_for_organization(&mut connection, member.organization.id)
            .await
            .map_err(log_internal)?;

    // Cloned into the response: a plan is a handful of strings, and the
    // alternative is a body that borrows the request's own state.
    let plan = resolve_plan(&state.plans, subscription.as_ref()).clone();

    Ok(Json(BillingBody {
        plan,
        subscription,
        billing_enabled: state.stripe.is_some(),
    }))
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
    let stripe = configured(&state)?;

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
    // it is the screen Stripe keeps current. Starting a second subscription
    // here would bill the organization twice.
    if Subscription::current_for_organization(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "This organization already has a subscription. Change it in the billing portal.",
        ));
    }

    let customer_id = ensure_customer(&mut connection, stripe, &member).await?;
    let landing = return_url(&state.app_url, member.organization.id);
    // `{CHECKOUT_SESSION_ID}` is a placeholder Stripe substitutes on the way
    // back, which is what lets the return page name the session that just
    // completed. The doubled braces escape it past this format string.
    let success_url = format!("{landing}?checkout=success&session_id={{CHECKOUT_SESSION_ID}}");
    let cancel_url = format!("{landing}?checkout=canceled");

    let session = stripe
        .create_checkout_session(NewCheckoutSession {
            customer_id: &customer_id,
            price_id: price.stripe_price_id(),
            quantity: DEFAULT_QUANTITY,
            success_url: &success_url,
            cancel_url: &cancel_url,
            organization_id: member.organization.id,
            plan_key: plan.key(),
        })
        .await
        .map_err(|error| stripe_failed(&error, "open a checkout session"))?;

    tracing::info!(
        organization.id = %member.organization.id,
        billing.plan = plan.key(),
        billing.interval = interval.as_str(),
        billing.checkout.session = %session.id,
        "checkout {{billing.checkout.session}} opened for {{billing.plan}} \
         ({{billing.interval}})",
    );

    Ok(Json(RedirectBody { url: session.url }))
}

/// Opens the Stripe customer portal for an organization that has a customer.
async fn open_portal(
    State(state): State<BillingState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    let stripe = configured(&state)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let customer_id = stored_customer_id(&mut connection, member.organization.id)
        .await?
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

/// The plan a subscription puts in force, or the free plan.
fn resolve_plan<'plans>(
    plans: &'plans PlanSet,
    subscription: Option<&Subscription>,
) -> &'plans Plan {
    // A subscription that grants nothing (incomplete, unpaid, paused) leaves
    // the organization on free, which is the same place a cancellation lands.
    let Some(subscription) = subscription.filter(|held| held.grants_access()) else {
        return plans.free();
    };

    plans.find(&subscription.plan_key).unwrap_or_else(|| {
        // The plan was renamed or removed while a subscription still named it.
        // Falling back to free keeps the page answering; the log is what tells
        // an operator to reconcile `config/billing.yml` with Stripe.
        tracing::error!(
            billing.plan = subscription.plan_key,
            organization.id = %subscription.organization_id,
            "subscription names plan {{billing.plan}}, which config/billing.yml no longer \
             defines; falling back to the free plan",
        );
        plans.free()
    })
}

/// The organization's Stripe customer, creating one on first use.
///
/// The write is conditional on the column still being null, so two checkouts
/// started at the same instant cannot overwrite each other. They cannot even
/// create two customers: the create call carries an idempotency key derived
/// from the organization, so Stripe answers the second one with the customer
/// the first one made.
async fn ensure_customer(
    connection: &mut AsyncPgConnection,
    stripe: &stripe::Client,
    member: &OrganizationMember,
) -> Result<String, ApiError> {
    if let Some(existing) = stored_customer_id(connection, member.organization.id).await? {
        return Ok(existing);
    }

    let customer = stripe
        .create_customer(NewCustomer {
            organization_id: member.organization.id,
            name: &member.organization.name,
            email: Some(&member.user.email),
        })
        .await
        .map_err(|error| stripe_failed(&error, "create a billing account"))?;

    diesel::update(
        organizations::table
            .find(member.organization.id)
            .filter(organizations::stripe_customer_id.is_null()),
    )
    .set(organizations::stripe_customer_id.eq(&customer.id))
    .execute(connection)
    .await
    .map_err(log_internal)?;

    // Re-read rather than returning what was just created: whoever won the
    // write is the customer this organization is billed as.
    stored_customer_id(connection, member.organization.id)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                organization.id = %member.organization.id,
                "the organization vanished between creating its Stripe customer and storing it",
            );
            ApiError::internal()
        })
}

/// The Stripe customer stored on an organization, if it has one.
async fn stored_customer_id(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> Result<Option<String>, ApiError> {
    organizations::table
        .find(organization_id)
        .select(organizations::stripe_customer_id)
        .first::<Option<String>>(connection)
        .await
        .optional()
        .map(Option::flatten)
        .map_err(log_internal)
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
    use super::{resolve_plan, return_url};
    use crate::billing::PlanSet;

    const PLANS: &str = "
plans:
  - key: free
    name: Free
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
";

    /// A subscription row shaped by hand, since the database is not involved.
    fn subscription(plan_key: &str, status: &str) -> crate::billing::Subscription {
        let now = chrono::Utc::now();
        crate::billing::Subscription {
            id: uuid::Uuid::new_v4(),
            organization_id: uuid::Uuid::new_v4(),
            plan_key: plan_key.to_owned(),
            stripe_subscription_id: "sub_test".to_owned(),
            status: status.to_owned(),
            billing_interval: "monthly".to_owned(),
            quantity: 1,
            current_period_end: None,
            cancel_at_period_end: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn no_subscription_resolves_to_the_free_plan() {
        let plans = PlanSet::from_yaml(PLANS).expect("the plans must parse");

        assert_eq!(resolve_plan(&plans, None).key(), "free");
    }

    #[test]
    fn a_paying_subscription_resolves_to_its_plan() {
        let plans = PlanSet::from_yaml(PLANS).expect("the plans must parse");

        for status in ["active", "trialing", "past_due"] {
            let held = subscription("pro", status);
            assert_eq!(
                resolve_plan(&plans, Some(&held)).key(),
                "pro",
                "for status {status}",
            );
        }
    }

    #[test]
    fn a_subscription_that_grants_nothing_resolves_to_free() {
        let plans = PlanSet::from_yaml(PLANS).expect("the plans must parse");

        for status in ["incomplete", "unpaid", "paused", "an_unknown_word"] {
            let held = subscription("pro", status);
            assert_eq!(
                resolve_plan(&plans, Some(&held)).key(),
                "free",
                "for status {status}",
            );
        }
    }

    #[test]
    fn a_plan_the_file_no_longer_defines_resolves_to_free() {
        let plans = PlanSet::from_yaml(PLANS).expect("the plans must parse");
        let held = subscription("legacy_gold", "active");

        assert_eq!(resolve_plan(&plans, Some(&held)).key(), "free");
    }

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
