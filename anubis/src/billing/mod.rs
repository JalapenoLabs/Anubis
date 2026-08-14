//! Subscription billing: plans in configuration, money at Stripe.
//!
//! Billing attaches to the **organization**, which is the tenant that owns
//! teams and pays for them (see `docs/tenancy.md`). An organization has at
//! most one subscription that is not over, and the plan it buys is defined in
//! the application's `config/billing.yml`, not in a database table.
//!
//! # The three pieces
//!
//! - [`PlanSet`] is the compiled `config/billing.yml`: the plans, their Stripe
//!   prices, and the limits each grants. It is validated at boot exactly as
//!   [`crate::roles::RoleSet`] is, so a bad edit fails the build rather than a
//!   customer's checkout.
//! - [`stripe::Client`] is the small REST client the framework talks to Stripe
//!   with: create a customer, open a checkout session, open a portal session.
//! - [`Subscription`] is the row mirroring Stripe's own record, which the
//!   application authorizes and renders from.
//!
//! [`router`] serves the organization-scoped endpoints an application mounts
//! under `/billing`.
//!
//! # The free plan is the absence of a row
//!
//! Exactly one plan in `billing.yml` sells no prices, and that is the free
//! plan. An organization with no subscription is on it. Nothing is written at
//! signup, nothing is backfilled when a plan is renamed, and a cancellation
//! lands back where a new organization starts. Plan resolution is therefore
//! total: every organization is always on a plan.
//!
//! # Stripe is the system of record for money
//!
//! The application never computes what a customer owes, never stores a card,
//! and never renders a plan-change screen: Stripe Checkout takes the purchase
//! and the Stripe Billing customer portal takes every change after it. What
//! this framework keeps is the projection it has to authorize from, and the
//! projection is only ever written from what Stripe reports.
//!
//! # Billing disabled
//!
//! `STRIPE_SECRET_KEY` is the switch. Unset, which is the development default,
//! the read endpoint keeps answering (every organization is on the free plan,
//! which is true) and the two write endpoints answer `503` naming the
//! variable. A production deployment with billing configured never notices;
//! one without it is warned at startup by [`crate::telemetry::init`].
//!
//! # What lands next
//!
//! Slab 1 is the foundation: plans, the client, the data model, and the
//! checkout and portal endpoints. Two pieces are deliberately not here yet,
//! and `docs/billing.md` states what each needs:
//!
//! - **Subscription lifecycle**: the incoming Stripe webhook that writes
//!   [`Subscription`] rows and keeps them current. Until it lands, a completed
//!   checkout charges the customer at Stripe and leaves this application
//!   showing the free plan.
//! - **Limit enforcement and the billing UI**: [`Plan::limit`] is read by
//!   nothing yet, and the frontend has no billing screen.

mod model;
pub mod plans;
mod routes;
pub mod stripe;

#[doc(inline)]
pub use model::{Subscription, SubscriptionStatus};
#[doc(inline)]
pub use plans::{Interval, Plan, PlanSet, Price};
#[doc(inline)]
pub use routes::{BILLING_ROLE, router};
