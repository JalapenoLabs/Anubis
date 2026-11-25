//! The organization-scoped billing endpoints.
//!
//! Eight routes, mounted by an application under `/billing`:
//!
//! | Route | Guard | Effect |
//! |---|---|---|
//! | `GET /organizations/{organization_id}` | org member | The plan in force and why, the plan subscription, the seats in use, and, for a member who may spend, every live subscription with its items and the wallet |
//! | `POST /organizations/{organization_id}/checkout` | `Billing::Subscription.update` | Opens a Stripe Checkout session and answers with its URL |
//! | `POST /organizations/{organization_id}/portal` | `Billing::Subscription.update` | Opens the Stripe customer portal and answers with its URL |
//! | `POST /organizations/{organization_id}/reconcile` | `Billing::Subscription.update` | Reads Stripe and corrects the subscriptions and wallet payments this application shows |
//! | `POST /organizations/{organization_id}/wallet/top-up` | `Billing::Subscription.update` | Opens a Stripe Checkout session that buys credit, and answers with its URL |
//! | `PUT /organizations/{organization_id}/wallet/auto-recharge` | `Billing::Subscription.update` | Turns auto-recharge on, or changes it |
//! | `DELETE /organizations/{organization_id}/wallet/auto-recharge` | `Billing::Subscription.update` | Turns auto-recharge off |
//! | `GET /organizations/{organization_id}/wallet/entries` | `Billing::Subscription.update` | One page of the wallet's ledger, newest first |
//!
//! Reading is open to every member because the plan and its limits explain
//! what the whole organization can do. Spending money is not: it takes
//! `Billing::Subscription.update`, which the administrator role always holds,
//! the framework grants the `billing` key, and an organization may hand to a
//! role of its own.
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

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use diesel_async::AsyncPgConnection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::currency;
use super::lifecycle::{self, Reconciler};
use super::limits::{self, PlanSource, Standing};
use super::model::{Subscription, SubscriptionWithItems};
use super::plans::{Interval, Plan, PlanSet, WalletTerms};
use super::stripe::{self, LineItem, NewPortalSession, Purchase};
use super::subscriptions::{self, Subscriptions, TopUp};
use super::wallet::{self, Wallet, WalletEntry};
use crate::audit;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::guard::OrganizationMember;
use crate::http::{ApiError, ListParams, Pagination};
use crate::roles::{Action, RoleSet, SUBSCRIPTION};
use crate::tenancy::OrganizationAccess;

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
        .route(
            "/organizations/{organization_id}/wallet/top-up",
            post(start_top_up),
        )
        .route(
            "/organizations/{organization_id}/wallet/auto-recharge",
            put(enable_auto_recharge).delete(disable_auto_recharge),
        )
        .route(
            "/organizations/{organization_id}/wallet/entries",
            get(list_entries),
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
    /// The plan in force: the plan subscription's, the paid plan, or the free plan.
    plan: Plan,
    /// Why that plan is in force, which every member may read beside it.
    plan_source: PlanSource,
    /// The plan subscription behind that plan, absent on the free plan.
    subscription: Option<SubscriptionWithItems>,
    /// Every live subscription the organization holds, each with its line
    /// items: the plan subscription first, then those paying for its records.
    ///
    /// `None` for a member who may not manage billing. The plan explains what
    /// the whole organization can do, which is why every member reads it, but
    /// a record's subscription names the record, its prices, and its status,
    /// and a member may not reach every record the organization pays for.
    subscriptions: Option<Vec<SubscriptionWithItems>>,
    /// Whether this deployment can start a checkout at all.
    billing_enabled: bool,
    /// People who can reach the organization, claimed and invited alike.
    ///
    /// The one usage figure the framework can count for every application,
    /// because memberships are its own table. A screen reads it against the
    /// plan's `seats` limit to show how full the organization is; an
    /// application's own limits are counted by the application.
    seats_used: i64,
    /// The prepaid credit wallet, when the application sells credit.
    ///
    /// `None` for a member who may not manage billing, for the same reason
    /// `subscriptions` is: the balance says what the organization spends, and
    /// spending is the business of a member who may spend.
    wallet: Option<WalletBody>,
}

/// The wallet as the billing screen renders it; every amount in micro-units.
#[derive(Serialize)]
struct WalletBody {
    /// The wallet's currency, lowercase ISO 4217.
    currency: String,
    /// The balance, which a refund can take below zero.
    balance_micros: i64,
    /// The smallest top-up, and the smallest recharge.
    minimum_top_up_micros: i64,
    /// The smallest step a charge moves in: a cent, or a whole yen.
    ///
    /// Sent rather than worked out in the browser, whose currency tables are
    /// display rules that disagree with Stripe's for some currencies.
    charge_step_micros: i64,
    /// The balance below which a spend tells the administrators.
    low_balance_micros: Option<i64>,
    /// The auto-recharge settings, when auto-recharge is on.
    auto_recharge: Option<AutoRechargeBody>,
    /// Whether a top-up saved a card that auto-recharge can charge.
    card_saved: bool,
    /// Whether a recharge is in flight.
    recharging: bool,
    /// When the last recharge failed, which is why auto-recharge is off.
    recharge_failed_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize)]
struct AutoRechargeBody {
    /// A spend that leaves the balance below this arms a recharge.
    threshold_micros: i64,
    /// What a recharge charges the saved card.
    amount_micros: i64,
}

#[derive(Deserialize)]
struct TopUpBody {
    /// What to buy, in micro-units: a whole cent at least the minimum top-up.
    amount_micros: i64,
    /// Whether to save the card, which is what auto-recharge charges.
    #[serde(default)]
    save_card: bool,
}

#[derive(Serialize)]
struct EntriesBody {
    entries: Vec<WalletEntry>,
    pagination: Pagination,
}

impl WalletBody {
    fn new(terms: &WalletTerms, wallet: Option<&Wallet>) -> Self {
        Self {
            // The wallet's own currency when it has one: its balance is
            // micro-units of that, whatever billing.yml names now.
            currency: wallet
                .map_or_else(|| terms.currency().to_owned(), |held| held.currency.clone()),
            balance_micros: wallet.map_or(0, |held| held.balance_micros),
            minimum_top_up_micros: terms.minimum_top_up_micros(),
            charge_step_micros: currency::charge_step_micros(terms.currency())
                .expect("billing.yml validation refuses a wallet currency with no charge step"),
            low_balance_micros: terms.low_balance_micros(),
            auto_recharge: wallet.and_then(Wallet::auto_recharge).map(
                |(threshold_micros, amount_micros)| AutoRechargeBody {
                    threshold_micros,
                    amount_micros,
                },
            ),
            card_saved: wallet.is_some_and(|held| held.recharge_payment_method_id.is_some()),
            recharging: wallet.is_some_and(|held| held.recharge_attempt_id.is_some()),
            recharge_failed_at: wallet.and_then(|held| held.recharge_failed_at),
        }
    }
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
    // Only what the caller may see is loaded: an organization paying per
    // record can hold hundreds of subscriptions, which a member who may not
    // spend on them is never shown.
    let list_all = holds_billing_authority(&member);
    let live = if list_all {
        Subscription::live_for_organization(&mut connection, member.organization.id).await
    } else {
        Subscription::current_for_organization(&mut connection, member.organization.id)
            .await
            .map(Option::into_iter)
            .map(Iterator::collect)
    }
    .map_err(log_internal)?;

    let body = overview(
        &mut connection,
        &state,
        member.organization.id,
        live,
        list_all,
    )
    .await?;
    Ok(Json(body))
}

/// The body both [`show`] and [`reconcile`] answer with.
///
/// `list_all` says whether the caller may see every subscription, rather than
/// the plan and the plan subscription alone.
async fn overview(
    connection: &mut AsyncPgConnection,
    state: &BillingState,
    organization_id: Uuid,
    live: Vec<Subscription>,
    list_all: bool,
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
    let standing = Standing::load(connection, &state.plans, organization_id)
        .await
        .map_err(log_internal)?;
    // Cloned into the response: a plan is a handful of strings, and the
    // alternative is a body that borrows the request's own state.
    let (plan, plan_source) = limits::plan_in_force(&state.plans, &standing);
    let wallet = match state.plans.wallet().filter(|_terms| list_all) {
        Some(terms) => {
            let held = Wallet::find(connection, organization_id)
                .await
                .map_err(log_internal)?;
            Some(WalletBody::new(terms, held.as_ref()))
        }
        None => None,
    };

    Ok(BillingBody {
        plan: plan.clone(),
        plan_source,
        subscription,
        subscriptions: list_all.then_some(subscriptions),
        billing_enabled: state.stripe.is_some(),
        seats_used,
        wallet,
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
    let body = overview(&mut connection, &state, member.organization.id, live, true).await?;
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

/// Opens a Stripe Checkout session that buys credit for the organization's wallet.
///
/// Nothing is credited here: the payment arrives as an event, and the amount
/// credited is the one Stripe received.
async fn start_top_up(
    State(state): State<BillingState>,
    member: OrganizationMember,
    Json(body): Json<TopUpBody>,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    let terms = wallet_terms(&state)?;
    configured(&state)?;

    // A wallet's currency is fixed, so credit in another one could never be
    // recorded: refused here, before Stripe takes the money for it.
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let held = Wallet::find(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?;
    drop(connection);
    if let Some(held) = held.filter(|held| held.currency != terms.currency()) {
        return Err(ApiError::conflict(format!(
            "This organization's credit is held in {}, and this deployment now sells credit in \
             {}. A wallet's currency cannot change, so no top-up can be taken.",
            held.currency.to_uppercase(),
            terms.currency().to_uppercase(),
        )));
    }

    if body.amount_micros < terms.minimum_top_up_micros() {
        return Err(ApiError::validation(format!(
            "A top-up buys at least {}.",
            currency::display(terms.minimum_top_up_micros(), terms.currency()),
        )));
    }
    let amount_minor =
        currency::to_minor(body.amount_micros, terms.currency()).ok_or_else(|| {
            ApiError::validation(format!(
                "Stripe cannot charge {} micro-units of {}: use whole cents, at most {} of them.",
                body.amount_micros,
                terms.currency(),
                currency::MAX_CHARGE_MINOR,
            ))
        })?;

    let landing = return_url(&state.app_url, member.organization.id);
    let session = state
        .subscriptions
        .open_top_up(
            member.organization.id,
            &TopUp {
                terms,
                amount_minor,
                save_card: body.save_card,
                landing: &landing,
                email: Some(&member.user.email),
            },
        )
        .await?;

    Ok(Json(RedirectBody { url: session.url }))
}

/// Turns auto-recharge on, or changes it, answering with the wallet.
async fn enable_auto_recharge(
    State(state): State<BillingState>,
    member: OrganizationMember,
    context: audit::Context,
    Json(body): Json<AutoRechargeBody>,
) -> Result<impl IntoResponse, ApiError> {
    let settings = Some((body.threshold_micros, body.amount_micros));
    change_auto_recharge(&state, &member, &context, settings).await
}

/// Turns auto-recharge off, answering with the wallet.
async fn disable_auto_recharge(
    State(state): State<BillingState>,
    member: OrganizationMember,
    context: audit::Context,
) -> Result<impl IntoResponse, ApiError> {
    change_auto_recharge(&state, &member, &context, None).await
}

/// The one write both auto-recharge routes make.
async fn change_auto_recharge(
    state: &BillingState,
    member: &OrganizationMember,
    context: &audit::Context,
    settings: Option<(i64, i64)>,
) -> Result<Json<WalletBody>, ApiError> {
    require_billing_authority(member)?;
    let terms = wallet_terms(state)?;
    configured(state)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let wallet = wallet::set_auto_recharge(
        &mut connection,
        &context.by(&member.user),
        terms,
        member.organization.id,
        settings,
    )
    .await?;

    Ok(Json(WalletBody::new(terms, Some(&wallet))))
}

/// One page of the wallet's ledger, newest first.
///
/// Open only to a member who may spend: a spend names what it paid for, and a
/// member may not reach every record the organization pays for.
async fn list_entries(
    State(state): State<BillingState>,
    member: OrganizationMember,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    require_billing_authority(&member)?;
    wallet_terms(&state)?;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let entries = WalletEntry::page(&mut connection, member.organization.id, &params)
        .await
        .map_err(log_internal)?;
    let total_items = WalletEntry::count(&mut connection, member.organization.id)
        .await
        .map_err(log_internal)?;

    Ok(Json(EntriesBody {
        entries,
        pagination: Pagination::new(&params, total_items),
    }))
}

/// The wallet's terms, or `404` for an application that sells no credit.
fn wallet_terms(state: &BillingState) -> Result<&WalletTerms, ApiError> {
    state.plans.wallet().ok_or_else(ApiError::not_found)
}

/// The Stripe client, or the honest answer that this deployment has none.
fn configured(state: &BillingState) -> Result<&stripe::Client, ApiError> {
    state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "Billing is not configured for this deployment. Set STRIPE_SECRET_KEY to enable it.",
        )
    })
}

/// Whether a member may spend the organization's money and see what it buys.
///
/// Spending is `Billing::Subscription.update`, which the administrator role
/// always holds and the `billing` key carries, and which an organization may
/// hand to a role of its own. It counts only for a full member or an
/// administrator: a guest reaches only what was granted by name, and the
/// organization's subscriptions pay for records across every sub-tenant, so a
/// guest's organization-wide roles spend nothing, as they manage nothing.
fn holds_billing_authority(member: &OrganizationMember) -> bool {
    let standing_counts =
        member.membership.access == OrganizationAccess::Full || member.access.is_administrator();
    standing_counts && member.can(Action::Update, SUBSCRIPTION)
}

/// Refuses a member who may see the plan but not buy one.
fn require_billing_authority(member: &OrganizationMember) -> Result<(), ApiError> {
    if holds_billing_authority(member) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You need permission to manage this organization's billing.",
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
