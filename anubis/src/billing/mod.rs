//! Subscription billing: plans in configuration, money at Stripe.
//!
//! Billing attaches to the **organization**, which is the tenant that owns
//! teams and pays for them (see `docs/tenancy.md`). An organization has at
//! most one subscription that is not over, and the plan it buys is defined in
//! the application's `config/billing.yml`, not in a database table.
//!
//! # The pieces
//!
//! - [`PlanSet`] is the compiled `config/billing.yml`: the plans, their Stripe
//!   prices, and the limits each grants. It is validated at boot exactly as
//!   [`crate::roles::RoleSet`] is, so a bad edit fails the build rather than a
//!   customer's checkout.
//! - [`stripe::Client`] is the small REST client the framework talks to Stripe
//!   with: create a customer, open a checkout session, open a portal session,
//!   and read subscriptions back.
//! - [`Subscription`] is the row mirroring Stripe's own record, which the
//!   application authorizes and renders from.
//! - [`StripeBillingEvent`] is one thing Stripe said, stored before anything is
//!   made of it, and [`Reconciler`] is the only thing that writes the
//!   projection from it.
//! - [`Limits`] is what makes a plan's limits mean something: it resolves the
//!   plan an organization is on and answers whether one more record fits.
//!
//! [`router`] serves the organization-scoped endpoints an application mounts
//! under `/billing`; [`webhook_router`] serves the receiver Stripe posts to,
//! mounted under `/webhooks`.
//!
//! # The lifecycle
//!
//! A purchase is a redirect to Stripe Checkout, so nothing about it comes back
//! through the browser that can be trusted. What is trusted is Stripe's own
//! events, and the loop is three steps:
//!
//! 1. [`webhook_router`] verifies the signature, stores the event, and queues
//!    [`ProcessStripeEvent`]. Nothing is decided inside the request.
//! 2. A worker runs the job through [`Reconciler::process`], which reads the
//!    subscription back from Stripe and writes the projection.
//! 3. [`Reconciler::reconcile`] repairs an organization whose events were
//!    missed, and is exposed as an endpoint for the day one is.
//!
//! Registering the two jobs is the whole of the wiring, beside the framework's
//! webhook deliverer:
//!
//! ```ignore
//! let reconciler = anubis::billing::Reconciler::new(pool.clone(), plans.clone(), &config);
//! let seats = reconciler.clone();
//! let worker = anubis::jobs::Worker::builder(pool)
//!     .register(move |job: anubis::billing::ProcessStripeEvent| {
//!         let reconciler = reconciler.clone();
//!         async move { reconciler.process(job).await }
//!     })
//!     .register(move |job: anubis::billing::SyncSeats| {
//!         let seats = seats.clone();
//!         async move { seats.sync_seats(job).await }
//!     })
//!     .build();
//! ```
//!
//! # Limits
//!
//! A plan's limits are configuration until something checks them. [`Limits`]
//! is that check, and the split is Bullet Train's: a hard limit refuses the
//! creation with `409`, a soft limit lets it through and leaves the screen to
//! report it. [`SEATS`] is the one limit the framework enforces itself, at
//! invitation creation, because memberships are its own table; every other
//! limit names something only the application can count, so the application
//! calls [`Limits::check`] at its own creation choke points.
//!
//! # Per-seat pricing
//!
//! A price marked `per_seat` in `billing.yml` is bought with the
//! organization's current seat count, and [`SyncSeats`] keeps that quantity
//! current: every membership change queues one, and it tells Stripe the new
//! number on Stripe's default proration terms.
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
//! which is true), the write endpoints answer `503` naming the variable, and
//! the receiver refuses events rather than believing unsigned ones.
//! `STRIPE_WEBHOOK_SECRET` is the second switch, and the receiver needs it for
//! the same reason: an event nobody signed is an event anybody could send. A
//! production deployment missing either is warned at startup by
//! [`crate::telemetry::init`].
//!
//! Limits are the exception: they are a product decision rather than a Stripe
//! one, so the free plan's limits hold whether or not Stripe is configured.

mod event;
pub mod lifecycle;
pub mod limits;
mod model;
pub mod plans;
mod routes;
pub mod stripe;
mod typescript;
mod webhook;

#[doc(inline)]
pub use event::StripeBillingEvent;
#[doc(inline)]
pub use lifecycle::{HANDLED_EVENTS, ProcessStripeEvent, QUEUE, Reconciler, SyncSeats};
#[doc(inline)]
pub use limits::{Limits, SEATS};
#[doc(inline)]
pub use model::{Subscription, SubscriptionStatus};
#[doc(inline)]
pub use plans::{Enforcement, Interval, Limit, Plan, PlanSet, Price};
#[doc(inline)]
pub use routes::{BILLING_ROLE, router};
#[doc(inline)]
pub use webhook::{WEBHOOK_PATH, WEBHOOK_SECRET_VAR, router as webhook_router};
