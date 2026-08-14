# Billing

Anubis bills organizations for subscription plans through Stripe. Plans live in one configuration file, money lives at Stripe, and the application keeps only the projection it has to authorize from.

This mirrors Bullet Train's billing experience (`bullet_train-billing` plus `bullet_train-billing-stripe`) with one file of products and prices, Stripe Checkout for the purchase, the Stripe Billing customer portal for every change after it, and per-plan usage limits. What differs is where things attach: Bullet Train subscribes a Team, Anubis subscribes an **Organization**, because the organization is the tenant that owns teams and pays for them. See [tenancy.md](tenancy.md).

## Status

The module lands in three slabs. This document marks what is built and what is not, so nothing here reads as a promise the code does not keep.

| Slab | What it covers | State |
|---|---|---|
| 1 | Plans in config, the Stripe client, the data model, checkout and portal endpoints | Built |
| 2 | Incoming Stripe webhooks and the subscription lifecycle | Built |
| 3 | Limit enforcement and the billing UI | Not built |

Until slab 3 lands, a plan's limits are stored and served but enforced by nothing, and the frontend has no billing screen. Run against Stripe's test mode until both are true.

## The model

- **Plans** are configuration, not rows. `config/billing.yml` names every plan, the Stripe price behind each billing interval, and the limits the plan grants. The backend embeds the file with `include_str!` and validates it at boot, exactly as it treats `config/roles.yml`.
- **A subscription** is a row in `subscriptions`, one per organization at a time, mirroring Stripe's own record: plan key, Stripe id, status, interval, quantity, period end, and whether it cancels at the end of the period.
- **The Stripe customer** is stored on the organization, in `organizations.stripe_customer_id`. It is null until the first checkout, so an organization that never pays never reaches Stripe at all.

### The free plan is the absence of a row

Exactly one plan in `billing.yml` sells no prices, and that is the free plan. An organization with no subscription is on it. Nothing is written at signup, nothing is backfilled when a plan is renamed, and a cancellation lands back where a new organization starts.

The payoff is that plan resolution is **total**: every organization is always on a plan, so no handler, no limit check, and no UI branch has to answer "what does an unsubscribed organization get". `PlanSet::free()` returns a plan rather than an option because validation guarantees there is one.

### Statuses

`status` holds Stripe's own vocabulary verbatim: `incomplete`, `incomplete_expired`, `trialing`, `active`, `past_due`, `canceled`, `unpaid`, `paused`. Translating it would only lose the distinctions support is asked about.

Two predicates read that column, and both are in `anubis::billing::SubscriptionStatus`:

- `is_terminal()` is `canceled` or `incomplete_expired`, the two statuses that mean the subscription is over. A partial unique index on `subscriptions (organization_id) WHERE status NOT IN ('canceled', 'incomplete_expired')` enforces **one live subscription per organization** in the database rather than in a handler, because two events arriving at once would otherwise each write a row and neither would know. Change the predicate and change the index in the same migration.
- `grants_access()` is `trialing`, `active`, or `past_due`. `past_due` counts on purpose: Stripe is still retrying the card, and cutting a paying customer off during a retry window is how a failed payment becomes a cancelled account. An organization whose subscription grants nothing falls back to the free plan.

## config/billing.yml

```yaml
plans:
  - key: free
    name: Free
    description: Everything one person needs to try the product.
    limits:
      seats: 1
      projects: 3

  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_1QpbQSKKAAAAAAAAAAAAAAAA
        amount: 2900
        currency: usd
      yearly:
        stripe_price_id: price_1QpbQSKKBBBBBBBBBBBBBBBB
        amount: 29000
        currency: usd
    limits:
      seats: 25
```

A sequence rather than a map, because file order is presentation order: a pricing page renders the plans as the file lists them.

`anubis::billing::PlanSet::from_yaml` rejects, with a message naming the plan:

- an empty plan list, or a document with unknown fields;
- a plan key or limit name that is not lowercase letters, digits, and underscores (they are stored on subscriptions and read by the frontend);
- a plan with no name, a negative amount, a negative limit, or a currency that is not a lowercase ISO 4217 code;
- a Stripe price id used by two plans, because a price is what identifies the plan a subscription bought;
- a plan list without **exactly one** free plan.

Plans deliberately do not inherit from one another the way roles do. Each states its limits in full, because a pricing table is read across and a limit inherited from a plan three rows up is a limit nobody can see. There is therefore no cycle to detect, only the duplicates above.

### Limits

The limit vocabulary is the application's own: `seats`, `projects`, whatever the product meters. The framework validates the shape and stores the numbers; slab 3 enforces them.

**A limit a plan does not name is unlimited.** That is the convention rather than a magic number, because "unlimited" written as `-1` is a value every caller has to remember to special-case.

### Amounts

`amount` and `currency` are what the pricing page shows, in the currency's smallest unit (2900 is $29.00). Stripe's price is the source of truth for what a customer is actually charged; only `stripe_price_id` is ever sent to Stripe. Keeping the amount in the file is what lets a pricing page render without an API call, and it is the same trade Bullet Train's `products.yml` makes.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `STRIPE_SECRET_KEY` | unset | Stripe secret key, `sk_live_...` or a restricted `rk_live_...`; setting it enables billing |
| `STRIPE_WEBHOOK_SECRET` | unset | Secret Stripe signs billing events with, `whsec_...`; setting it enables the receiver |
| `STRIPE_API_BASE` | `https://api.stripe.com` | Where Stripe's API lives; for tests and Stripe-compatible mocks only |

`STRIPE_SECRET_KEY` is the switch, and it follows the same posture as `SMTP_URL`:

- **Unset** (the development default): every organization is on the free plan, `GET` keeps answering with `billing_enabled: false`, and checkout and portal answer `503` naming the variable. The UI hides what cannot work.
- **Production without it**: the application boots and warns at startup. An application that does not charge yet should not be blocked on billing configuration.
- **Malformed**: a publishable key (`pk_...`) is refused by name at boot, because it is the key people reach for first and every call made with it would fail at Stripe.

`STRIPE_WEBHOOK_SECRET` is the second switch, and it governs the receiver alone:

- **Unset**: the receiver answers `503` naming the variable, and subscriptions stop being kept current. A production deployment with billing on and this off is warned at startup, because it is the one configuration that takes money and hears nothing back.
- **Set without `STRIPE_SECRET_KEY`**: refused at boot. The receiver reads each subscription back from Stripe, which needs the key, so half-configured billing never starts.
- **Set to an API key**: refused at boot by name. It is the paste people get wrong, and every real event would be rejected by a receiver that took it.

Both values are bearer credentials for an account that moves money, so neither is ever echoed in an error, a log line, or a `Debug` rendering.

`anubis doctor` validates `config/billing.yml` with the parser the server boots with, and reports its absence as "billing is off for this application" rather than as a problem.

## Endpoints

Two mounts, both in the application's `main.rs` and both listed by `anubis routes`: the organization-scoped surface under `/billing`, and the receiver under `/webhooks`.

| Route | Guard | Effect |
|---|---|---|
| `GET /billing/organizations/{organization_id}` | org member | The plan in force, the subscription behind it, and whether billing is configured |
| `POST /billing/organizations/{organization_id}/checkout` | org `admin` or `billing` | Opens a Stripe Checkout session, answers with its URL |
| `POST /billing/organizations/{organization_id}/portal` | org `admin` or `billing` | Opens the Stripe customer portal, answers with its URL |
| `POST /billing/organizations/{organization_id}/reconcile` | org `admin` or `billing` | Reads Stripe and corrects what this application shows |
| `POST /webhooks/stripe-billing` | a Stripe signature | Stores one Stripe event and queues the job that acts on it |

Reading is open to every member of the organization because the plan and its limits explain what the whole organization can do. Spending money takes the `admin` role or the `billing` role, which the starter's `roles.yml` ships described as "can manage billing and subscriptions"; repairing the projection takes the same authority, because it is the same subject. A member of another organization gets `404` on all four, the same as a member of none: the `OrganizationMember` guard never reveals that an organization exists.

The receiver is the exception to all of that: it is mounted under `/webhooks` rather than `/billing`, carries no guard, and authenticates its caller with a signature. See [the lifecycle](#the-subscription-lifecycle).

`GET` answers:

```json
{
  "plan": {
    "key": "pro",
    "name": "Pro",
    "description": null,
    "highlighted": true,
    "prices": { "monthly": { "stripe_price_id": "price_...", "amount": 2900, "currency": "usd" } },
    "limits": { "seats": 25 }
  },
  "subscription": {
    "id": "…", "organization_id": "…", "plan_key": "pro",
    "stripe_subscription_id": "sub_…", "status": "active",
    "billing_interval": "monthly", "quantity": 1,
    "current_period_end": "2026-09-13T00:00:00Z", "cancel_at_period_end": false,
    "created_at": "…", "updated_at": "…"
  },
  "billing_enabled": true
}
```

`POST .../checkout` takes `{"plan_key": "pro", "interval": "monthly"}` and answers `{"url": "https://checkout.stripe.com/…"}`. It refuses, in this order:

| Answer | When |
|---|---|
| `403` | The caller holds neither `admin` nor `billing` |
| `503` | `STRIPE_SECRET_KEY` is unset |
| `400` | Unknown plan, the free plan, an unknown interval, or an interval that plan does not sell |
| `409` | The organization already has a live subscription; changing plans is the portal's job |

`POST .../portal` answers `409` when the organization has no Stripe customer yet, because there is nothing to manage before the first purchase.

Both write endpoints answer `503` when Stripe cannot be reached, which is a temporary state worth retrying, and `500` when Stripe refuses the call, which is a misconfiguration on this side (a price id that no longer exists, an account that cannot take payments) with the detail in the log.

### Return URLs

Stripe needs absolute URLs before a purchase begins, so the framework builds them from `APP_URL`: `<APP_URL>/organizations/<organization_id>/billing`, with `?checkout=success&session_id=…` or `?checkout=canceled` on the two checkout outcomes. The path is scoped to the organization because that is the page the browser should land on, and it deliberately avoids `/billing`, which is a reserved API prefix (see [architecture.md](architecture.md)): a client route there would answer JSON on a cold load.

## The subscription lifecycle

A purchase happens at Stripe, in a browser tab this application does not control, so nothing that comes back through the browser can be trusted. What is trusted is Stripe's own events, and the loop they drive is three steps: **store**, **process**, **reconcile**.

### The receiver

Stripe posts to `POST /webhooks/stripe-billing`.

It is **framework-mounted**, not scaffolded, because the subscription lifecycle is the framework's own feature: `anubis::billing::webhook_router` is nested under `/webhooks` by the application's `main.rs`, beside whatever `anubis scaffold webhook` generated for the application itself.

**The path is the coexistence story.** `anubis scaffold webhook Stripe` generates an application-owned receiver at `/webhooks/stripe`, for Stripe Connect, one-off payments, or anything else the framework knows nothing about. That receiver has its own table, its own job, and its own `STRIPE_WEBHOOK_SECRET`-shaped variable, and it never sees a billing event, because the two are separate endpoints registered separately in the Stripe dashboard. `anubis scaffold webhook StripeBilling` is refused by name, since two routers claiming one path panic at boot.

The endpoint does two things and no more: it writes the event down and queues a job, both in one transaction, then answers `200`. That is the store-then-process discipline [webhooks.md](webhooks.md) describes for generated receivers, and the reason is the same: work done inside the request can time out and be retried, work done after the `200` is the queue's, with its own backoff and its own dead-letter table.

| Column | What it holds |
|---|---|
| `stripe_event_id` | Stripe's own `evt_...`, **unique**, which is the whole idempotency guarantee |
| `event_type` | e.g. `customer.subscription.updated`, so the backlog reads without opening every document |
| `payload` | Stripe's JSON, exactly as it arrived |
| `stripe_created_at` | When Stripe created the event, which is how ordering is decided |
| `received_at` | When the request arrived |
| `processed_at` | When processing finished, or `NULL` while the row is still waiting |
| `error` | The most recent processing failure |

Stripe redelivers an event it did not hear a `2xx` for, sometimes while the first delivery is still being processed. The unique index is what turns two concurrent redeliveries into one row and one job; the endpoint answers `200` with the same row id both times, and queues nothing the second time.

### Verification is strict, and refused requests are not stored

The signature is Stripe's own scheme: `Stripe-Signature: t=<unix>,v1=<hex>`, where the signed message is `<t>.<body>` over the exact bytes that arrived. `anubis::webhooks::signature::verify_stripe` is that check, and it lives in the provider-agnostic signature module so an application receiving Stripe events for its own purposes does not write the parser a second time. It accepts several `v1` entries, which is what makes a secret rotation at Stripe seamless, ignores schemes it does not know, and refuses a timestamp further than five minutes from now, which is what stops a captured request from being replayed.

A request that does not verify is refused with `400`, per Stripe's own guidance, and **nothing is stored**. This is the one place the framework departs from what a generated receiver does, and the difference is knowledge: a generated `verify_signature` is a function the developer has to finish, so a check that is subtly wrong must not look like silence, and storing the request is the evidence that explains it. Here the scheme is known, implemented, and tested, so a request that fails is either an attacker or a mistyped secret. Neither is worth a row, the response message names the reason where Stripe's delivery log shows it, and a public unauthenticated endpoint that writes a row for anything that arrives is a liability rather than a diary.

With `STRIPE_WEBHOOK_SECRET` unset the endpoint answers `503` naming the variable, rather than `200`. A silent drop would look to Stripe like success and the events would never be redelivered.

### The events, and what each one writes

`anubis::billing::HANDLED_EVENTS` is the list to select when creating the endpoint at Stripe:

| Event | What the framework does |
|---|---|
| `checkout.session.completed` | Resolves the organization, reads the new subscription back from Stripe, and upserts the row |
| `customer.subscription.created` | Same, for a subscription created anywhere else, including the Stripe dashboard |
| `customer.subscription.updated` | Same: status, period end, quantity, `cancel_at_period_end`, and a plan change through the portal |
| `customer.subscription.deleted` | Writes the terminal status from the event's own body |

Every other event type is **stored, marked processed, and ignored**, which is what makes "send all events" a safe way to configure the endpoint. A receiver that failed on the types it does not know would fail on every feature Stripe ships.

**The organization** comes from the subscription's `metadata[organization_id]`, which checkout put there, then from the checkout session's own metadata, and finally from `organizations.stripe_customer_id`, which is the mapping this application wrote itself when the customer was created. That last fallback is what makes a subscription started in the Stripe dashboard land correctly. An event that resolves to no organization fails the job and stays on the row as evidence.

**The plan** comes from the price, through `PlanSet::find_by_price_id`, because changing plan in the customer portal changes the price and leaves the metadata naming what was bought first. A price `config/billing.yml` does not sell falls back to the plan the metadata names, and if that fails too the job fails with a message naming the price: the fix is to add the price to the file or to stop offering the product in the portal.

### Reading rather than trusting

An event's body is a snapshot of the moment Stripe created it, and a job may run it minutes later. Stripe's guidance is to treat the payload as possibly stale, so the three non-terminal events above are treated as **notifications**: the event says which subscription changed, and `GET /v1/subscriptions/{id}` says what it looks like now. That is the fifth call on the Stripe client, and it is the honest one.

`customer.subscription.deleted` is the exception, and it is trusted as it arrives. A cancelled subscription is over, nothing Stripe could say about it later would be newer, and re-reading it would spend a call to be told what the event already said.

### Out-of-order delivery

Stripe does not promise order. Every write carries the creating event's `created` timestamp into `subscriptions.stripe_event_at`, and the upsert applies only when the incoming event is **not older** than the one already applied:

```sql
INSERT INTO subscriptions (...) VALUES (...)
ON CONFLICT (stripe_subscription_id) DO UPDATE SET ...
WHERE subscriptions.stripe_event_at IS NULL
   OR subscriptions.stripe_event_at <= excluded.stripe_event_at
```

So a late `customer.subscription.updated` cannot undo a cancellation that arrived first. Two events created in the same second both apply, which is safe precisely because each one writes the state read back from Stripe rather than its own body. Reconciliation stamps `now`, because it read Stripe directly and is therefore at least as current as anything in flight.

The **one live subscription per organization** index from slab 1 holds under all of this: the upsert is keyed on `stripe_subscription_id`, so a resubscription after a cancellation inserts a second row while the first is terminal, and a customer who somehow holds two live subscriptions at Stripe fails the job with a message saying so, because that is a decision a person has to make.

### Processing

The job is `anubis::billing::ProcessStripeEvent`, on its own `billing` queue so that Stripe replaying a day of events, or a Stripe API call that hangs, cannot hold up the work a person is waiting on. `anubis::billing::Reconciler` is the handler, and an application registers it on its worker beside the webhook deliverer:

```rust
let reconciler = anubis::billing::Reconciler::new(pool.clone(), plans.clone(), &config);
let worker = anubis::jobs::Worker::builder(pool.clone())
    .register(move |job: anubis::billing::ProcessStripeEvent| {
        let reconciler = reconciler.clone();
        async move { reconciler.process(job).await }
    })
    .build();
```

Delivery is at-least-once, like every job on this queue, so the handler is idempotent twice over: an event already stamped `processed_at` returns without acting, and the write itself is an upsert. A failure records the message on the event row **and** returns the error, so the queue retries on its widening backoff and eventually dead-letters the job with the row still holding the reason. See [jobs.md](jobs.md).

### Reconciliation

A missed event would otherwise leave the projection wrong forever: an endpoint misconfigured for a week, a deployment down while Stripe gave up retrying, a price that reached `config/billing.yml` after the first customer bought it.

`Reconciler::reconcile(organization_id)` is the repair, and it is the same convergence the events drive. It reads every subscription Stripe holds for the organization's customer (`GET /v1/subscriptions?customer=…&status=all`, the sixth client call), writes each one, and answers with the organization's live subscription afterwards. Finished subscriptions are written first, so a row this application still believes is live releases the slot before the live one takes it, and the whole pass runs in one transaction, so a subscription that cannot be recorded leaves the projection as it was.

`POST /billing/organizations/{id}/reconcile` exposes it, guarded like the other writes, and answers with the same body as the `GET`, so the screen that triggered it re-renders from the answer. An organization that never reached Stripe has nothing to converge and answers with what it already had. Being unable to reach Stripe answers `503`; anything else answers `500` with the detail in the log.

There is no periodic sweep yet, because there are no recurring schedules yet; that is tracked with the rest of the scheduling work in [jobs.md](jobs.md#roadmap). The function is written to be called from one the day it exists.

## Why a hand-written Stripe client

`anubis::billing::stripe::Client` is roughly 200 lines over the `reqwest` client the framework already carries. The alternative considered was the `async-stripe` crate.

- **Surface area**: the whole integration is five calls, three form-encoded POSTs and two reads, every one answering flat JSON. A generated SDK models the entire Stripe API to serve five calls.
- **Pure-Rust TLS is mandatory** here, and both options can satisfy it, but `reqwest` with rustls is already in the tree for outgoing webhook delivery, so this adds no dependency and no second TLS stack. `cargo tree -i openssl` matches nothing, and CI keeps it that way.
- **Compile time**: this workspace compiles in seconds and that is worth protecting. `async-stripe` is one of the larger crates in the ecosystem, and its API surface changes with Stripe's.
- **The escape hatch stays open**: the client is one module behind a small surface. An application that needs more of Stripe than the framework uses can depend on `async-stripe` itself without fighting anything here.

Conventions the client holds to:

- Requests are `application/x-www-form-urlencoded` with Stripe's nested parameter syntax (`metadata[organization_id]`, `line_items[0][price]`).
- Responses are parsed into the few fields the framework reads. Stripe adds fields constantly, and a struct insisting on all of them would break on their schedule rather than ours.
- Creating a customer carries an `Idempotency-Key` derived from the organization, so two checkouts started at the same instant converge on one customer instead of racing to create two. The store is then conditional on the column still being null, so neither request can overwrite the other.
- Checkout sessions carry `client_reference_id`, `metadata[organization_id]`, `metadata[plan_key]`, and the same two values under `subscription_data[metadata]`, so every event that follows identifies the tenant without a lookup table of Stripe ids.
- No `Stripe-Version` header is sent, so calls run on the account's default API version. Pinning one is a single constant when a deployment wants its upgrades to be deliberate. Because the version is the account's, a subscription's `current_period_end` is read from the subscription and from its first line item, which is where Stripe moved it in the 2025 versions.
- Ids that arrive in an event body are percent-encoded before they reach a path, because a path is never a place to paste input.

## Setting Stripe up

1. Create the products and prices in the Stripe dashboard (start in test mode: <https://dashboard.stripe.com/test/products>).
2. Copy each price id into `config/billing.yml`.
3. Put the secret key in `.env` as `STRIPE_SECRET_KEY` (<https://dashboard.stripe.com/test/apikeys>).
4. Activate the customer portal and choose which products it may switch between (<https://dashboard.stripe.com/test/settings/billing/portal>). List only plans this application defines, or a plan change will produce an event naming a price no plan sells.
5. Add a webhook endpoint at `<APP_URL>/webhooks/stripe-billing` (<https://dashboard.stripe.com/test/webhooks/create>), selecting the four events in `anubis::billing::HANDLED_EVENTS`. Selecting every event works too; the rest are stored and ignored.
6. Copy that endpoint's signing secret into `.env` as `STRIPE_WEBHOOK_SECRET`.
7. Repeat all of it in live mode before launch, including a live `STRIPE_SECRET_KEY` and the live endpoint's own signing secret. The two secrets are per mode and per endpoint, so nothing carries over.

In development, Stripe cannot reach `localhost`. Forward the events with `stripe listen --forward-to localhost:3000/webhooks/stripe-billing`, which prints a signing secret of its own for `STRIPE_WEBHOOK_SECRET`, or expose the port with a tunnel and register the tunnel's URL.

## Testing

Two narratives, both against a **mock Stripe**: a small axum server serving the endpoints the framework calls, pointed at with `STRIPE_API_BASE`, exactly as `oauth_flow.rs` serves a mock OpenID Connect provider. Nothing in the code under test is special-cased for a test.

- `anubis/tests/billing_flow.rs` covers the purchase: the plan resolved from configuration, the customer created and stored, the checkout session opened with the price the plan names, and the portal. The assertions read the real request bodies, down to the idempotency key and the `Authorization` header.
- `anubis/tests/billing_webhooks_flow.rs` covers the lifecycle: events signed with Stripe's scheme and the configured secret, a refused signature (missing, wrong, tampered, replayed) storing nothing, a redelivery becoming one row and one job, a full lifecycle from completed checkout through a plan change to cancellation and a fresh subscription, an out-of-order event changing nothing, reconciliation correcting a drifted row, and two organizations that never see each other's subscriptions.

The lifecycle suite proves the retrieve-rather-than-trust decision directly: the event bodies it delivers carry a status the mock no longer agrees with, and the row that lands is the mock's.

Both use `TestDatabase`, so each narrative owns a database and can count rows; see [testing.md](testing.md).

## What slab 3 needs

- `Plan::limits()` and `Plan::limit(name)` are the whole limit surface, already validated.
- `PlanSet` is a plain, ordered, serializable structure, so a `anubis billing generate-ts` emitter renders the plan catalog into the SPA the way `anubis roles generate-ts` renders permissions. The frontend then needs no endpoint to draw a pricing page.
- `GET /billing/organizations/{id}` already answers with the plan, the subscription, and `billing_enabled`, which is what a billing screen renders.
- The client route to add is `/organizations/:organizationId/billing`, which is where Stripe returns the browser.

Open questions slab 3 has to settle:

- **Hard versus soft limits.** Bullet Train marks each limit `hard` (the form is disabled) or `soft` (the user is warned). The current schema is `name: count`; adding enforcement means either a nested shape or a naming convention, and the choice is slab 3's.
- **Per-seat pricing.** `quantity` exists on the line item and on the row, and the lifecycle keeps the row's copy current from what Stripe reports, but checkout sends `1`. Charging per membership means deciding what counts as a seat and telling Stripe when the count changes.
- **Notifying a customer whose card failed.** `past_due` reaches the row through `customer.subscription.updated` and keeps access on purpose, but nobody is told. The event to hang that on is `invoice.payment_failed`, and the surface it needs is email or in-app notification rather than billing.
- **Time-based limits.** Bullet Train's usage trackers meter verbs over a window. That is a larger feature than a count and may not be worth it.
