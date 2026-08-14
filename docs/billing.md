# Billing

Anubis bills organizations for subscription plans through Stripe. Plans live in one configuration file, money lives at Stripe, and the application keeps only the projection it has to authorize from.

This mirrors Bullet Train's billing experience (`bullet_train-billing` plus `bullet_train-billing-stripe`) with one file of products and prices, Stripe Checkout for the purchase, the Stripe Billing customer portal for every change after it, and per-plan usage limits. What differs is where things attach: Bullet Train subscribes a Team, Anubis subscribes an **Organization**, because the organization is the tenant that owns teams and pays for them. See [tenancy.md](tenancy.md).

## The module

Everything below is built: plans in configuration, the Stripe client, checkout and the customer portal, the webhook lifecycle and reconciliation, limit enforcement with per-seat pricing, and the billing screen. Run against Stripe's test mode until a live key and a live endpoint are configured.

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
    description: Everything a small group needs to try the product.
    limits:
      seats: 3
      projects: 3

  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_1QpbQSKKAAAAAAAAAAAAAAAA
        amount: 2900
        currency: usd
        per_seat: true
      yearly:
        stripe_price_id: price_1QpbQSKKBBBBBBBBBBBBBBBB
        amount: 29000
        currency: usd
        per_seat: true
    limits:
      seats: 25
      projects:
        count: 100
        enforcement: soft
```

A sequence rather than a map, because file order is presentation order: a pricing page renders the plans as the file lists them.

`anubis::billing::PlanSet::from_yaml` rejects, with a message naming the plan:

- an empty plan list, or a document with unknown fields;
- a plan key or limit name that is not lowercase letters, digits, and underscores (they are stored on subscriptions and read by the frontend);
- a plan with no name, a negative amount, a negative limit, or a currency that is not a lowercase ISO 4217 code;
- a Stripe price id used by two plans, because a price is what identifies the plan a subscription bought;
- a plan list without **exactly one** free plan.

Plans deliberately do not inherit from one another the way roles do. Each states its limits in full, because a pricing table is read across and a limit inherited from a plan three rows up is a limit nobody can see. There is therefore no cycle to detect, only the duplicates above.

### Amounts

`amount` and `currency` are what the pricing page shows, in the currency's smallest unit (2900 is $29.00). Stripe's price is the source of truth for what a customer is actually charged; only `stripe_price_id` is ever sent to Stripe. Keeping the amount in the file is what lets a pricing page render without an API call, and it is the same trade Bullet Train's `products.yml` makes.

## Limits

The limit vocabulary is the application's own: `seats`, `projects`, whatever the product meters. Names are lowercase words with underscores, and plural reads best, because a refusal renders the name verbatim ("The Free plan's projects limit of 3 is reached").

**A limit a plan does not name is unlimited.** That is the convention rather than a magic number, because "unlimited" written as `-1` is a value every caller has to remember to special-case.

### Hard and soft

A limit written as a bare number is **hard**. Writing it as a mapping opts into **soft**:

```yaml
limits:
  seats: 25                # hard: the invitation past it is refused
  projects:
    count: 100
    enforcement: soft      # allowed past it, and reported as over
```

The two spellings mean the same count, so the terse one stays the common case and the verbose one is only paid for where the choice is being made. The split is Bullet Train's (`enforcement: hard | soft`), and so is the behavior: a hard limit disables the create, a soft one warns and lets it through.

- **Hard**: `Limits::check` answers `409 Conflict` with a message naming the plan, the limit, and the number: *"The Free plan's projects limit of 3 is reached. Upgrade to add more."* `409` rather than `403`, because the request is well formed and only the organization's current plan refuses it. The message is written for a customer and is returned verbatim, which is what makes it worth upgrading over.
- **Soft**: the check returns `Ok`, the record is created, and the screen reports the overage. The plan's limits carry their `enforcement` into the API body and into the generated TypeScript, so the UI knows which of the two it is looking at without asking.

### Seats are the framework's; everything else is the application's

`seats` is the one limit the framework meters and enforces itself, at the one place a person joins an organization: **creating an invitation**. It has to be, because memberships are the framework's own table and no application code runs there. The check is inside the transaction that writes the invitation, after locking the organization row, so two admins inviting at the same instant are ordered rather than each seeing room for one more.

**A seat is a person, counted once.** `anubis::billing::Limits::seats_used` counts the distinct email addresses that can reach an organization: everyone holding an organization membership, everyone holding a claimed membership in one of its teams, and every invitation still claimable. Counting addresses rather than rows is what keeps one person who belongs to three teams from paying for three seats, and what makes re-inviting somebody who already holds a seat free. An expired invitation holds no seat: it can no longer be claimed, so charging for it would bill for nothing and block an admin who cannot see why.

Every other limit names something only the application can count, so the application calls the check at its own creation choke points:

```rust
let limits = anubis::billing::Limits::new(plans.clone());

// In the create handler, before the insert:
let held = Project::count_for_team(&mut connection, team.id).await?;
limits.check(&mut connection, organization_id, "projects", held).await?;
```

`check` takes the caller's own connection rather than a pool, which is what lets it run inside the transaction that is about to insert: a limit read outside the write it guards is a limit two requests can pass at once. `anubis::billing::limits::Error` converts into `ApiError`, so the call is one line and a `?`.

The scaffolder does not write that line yet: the limit a model is metered by is a product decision (the name, whether it is metered at all, whether the count is per team or per organization) and a template that guessed would generate a check nobody asked for. See [scaffolding.md](scaffolding.md#plan-limits).

## Per-seat pricing

A price marked `per_seat: true` is charged per seat rather than per organization. Three things follow, and nothing else changes:

1. **Checkout buys the seats in use.** The line item's quantity is `seats_used` at the moment of purchase, never below one, so the first invoice is right without waiting for a membership to change.
2. **Membership changes update Stripe.** Creating, claiming, or revoking an invitation, removing a member, leaving, and deleting a team each queue `anubis::billing::SyncSeats` on the `billing` queue, inside the transaction that made the change. A change that rolls back queues nothing. The job carries the organization and nothing else, so a burst of invitations converges on one number rather than replaying every intermediate one.
3. **The job is a no-op unless it has work.** It returns without calling Stripe when billing is off, when the organization has no subscription that grants access, when the plan's price is not per-seat, or when the quantity is already right. `PlanSet::sells_per_seat` is checked before queueing at all, so a flat-priced application never writes a job row.

The update is `POST /v1/subscriptions/{id}` with `items[0][id]` and `items[0][quantity]`, because Stripe carries the quantity on the line item rather than on the subscription. Nothing about proration is sent, which leaves **Stripe's default, `create_prorations`**: a seat added mid-period is charged for the rest of that period, and a seat removed earns a credit against the next invoice. That is what customers expect from per-seat billing, and changing it is an account-level decision rather than a framework one.

The answer Stripe gives is written straight into the projection, stamped `now`, exactly as reconciliation is: the row a screen renders agrees with the invoice the customer will get.

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
| `GET /billing/organizations/{organization_id}` | org member | The plan in force, the subscription behind it, the seats in use, and whether billing is configured |
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
    "prices": {
      "monthly": {
        "stripe_price_id": "price_…", "amount": 2900, "currency": "usd", "per_seat": true
      }
    },
    "limits": { "seats": { "count": 25, "enforcement": "hard" } }
  },
  "subscription": {
    "id": "…", "organization_id": "…", "plan_key": "pro",
    "stripe_subscription_id": "sub_…", "status": "active",
    "billing_interval": "monthly", "quantity": 2,
    "current_period_end": "2026-09-13T00:00:00Z", "cancel_at_period_end": false,
    "created_at": "…", "updated_at": "…"
  },
  "billing_enabled": true,
  "seats_used": 2
}
```

`seats_used` is the one usage figure the framework can count for every application, because memberships are its own table. A screen reads it against the plan's `seats` limit to show how full the organization is; an application's own limits are counted by the application.

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

An event's body is a snapshot of the moment Stripe created it, and a job may run it minutes later. Stripe's guidance is to treat the payload as possibly stale, so the three non-terminal events above are treated as **notifications**: the event says which subscription changed, and `GET /v1/subscriptions/{id}` says what it looks like now. That is one of the two reads on the Stripe client, and it is the honest one.

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

`Reconciler::reconcile(organization_id)` is the repair, and it is the same convergence the events drive. It reads every subscription Stripe holds for the organization's customer (`GET /v1/subscriptions?customer=…&status=all`, the other read), writes each one, and answers with the organization's live subscription afterwards. Finished subscriptions are written first, so a row this application still believes is live releases the slot before the live one takes it, and the whole pass runs in one transaction, so a subscription that cannot be recorded leaves the projection as it was.

`POST /billing/organizations/{id}/reconcile` exposes it, guarded like the other writes, and answers with the same body as the `GET`, so the screen that triggered it re-renders from the answer. An organization that never reached Stripe has nothing to converge and answers with what it already had. Being unable to reach Stripe answers `503`; anything else answers `500` with the detail in the log.

There is no periodic sweep yet, because there are no recurring schedules yet; that is tracked with the rest of the scheduling work in [jobs.md](jobs.md#roadmap). The function is written to be called from one the day it exists.

## Why a hand-written Stripe client

`anubis::billing::stripe::Client` is roughly 200 lines over the `reqwest` client the framework already carries. The alternative considered was the `async-stripe` crate.

- **Surface area**: the whole integration is six calls, four form-encoded POSTs and two reads, every one answering flat JSON. A generated SDK models the entire Stripe API to serve six calls.
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

Three narratives, all against a **mock Stripe**: a small axum server serving the endpoints the framework calls, pointed at with `STRIPE_API_BASE`, exactly as `oauth_flow.rs` serves a mock OpenID Connect provider. Nothing in the code under test is special-cased for a test.

- `anubis/tests/billing_flow.rs` covers the purchase: the plan resolved from configuration, the customer created and stored, the checkout session opened with the price the plan names, and the portal. The assertions read the real request bodies, down to the idempotency key and the `Authorization` header.
- `anubis/tests/billing_webhooks_flow.rs` covers the lifecycle: events signed with Stripe's scheme and the configured secret, a refused signature (missing, wrong, tampered, replayed) storing nothing, a redelivery becoming one row and one job, a full lifecycle from completed checkout through a plan change to cancellation and a fresh subscription, an out-of-order event changing nothing, reconciliation correcting a drifted row, and two organizations that never see each other's subscriptions.

- `anubis/tests/billing_limits_flow.rs` covers the limits: an invitation past the free plan's seats refused with the plan named, a re-invitation of somebody who already holds a seat costing nothing, a soft limit letting a record through where a hard one refuses (both against a real subscription row, so plan resolution is exercised rather than assumed), a membership change queueing `SyncSeats` only when a price is per-seat, and the job telling a stateful mock Stripe the new quantity exactly once.

The lifecycle suite proves the retrieve-rather-than-trust decision directly: the event bodies it delivers carry a status the mock no longer agrees with, and the row that lands is the mock's.

All three use `TestDatabase`, so each narrative owns a database and can count rows; see [testing.md](testing.md). On the frontend, `BillingPage.test.tsx` renders the screen against a stubbed `fetch` in three states: billing disabled, the free plan with its grid, and a live subscription set to cancel.

## The plan catalog in the frontend

```
anubis billing generate-ts --file config/billing.yml --out frontend/src/plans.generated.ts
```

The fourth generator, beside `roles generate-ts` and the two clients, and it follows the same discipline: deterministic output in house style, committed by the application, regenerated in CI and compared with `git diff --exit-code`. A `billing.yml` edit that was never regenerated fails there instead of in review.

It emits the ordered plans with their prices, limits, `highlighted`, and `free` flags, plus `PlanKey`, `FREE_PLAN_KEY`, and `findPlan(key)`. The pricing grid reads that file rather than an endpoint, because the catalog is configuration compiled into the same build: asking the server for it would be a request for a file that shipped with the page. The API's `plan` is a different thing and stays an endpoint, because it is the one plan an organization is actually on.

## The billing screen

`/organizations/:organizationId/billing` in the starter, which is where Stripe returns the browser, so a cold load has to work (and is why the path avoids `/billing`, a reserved API prefix). The organization's settings page links to it, and so does the tenancy menu.

It renders in one shape with a few states:

| State | What the screen shows |
|---|---|
| Billing disabled | The current plan card, and a card naming `STRIPE_SECRET_KEY` instead of a pricing grid. Nothing that cannot work is offered. |
| Free plan | The plan, its seat usage, no subscription chip, and the full grid with the free plan marked current |
| Active subscription | A status chip, the renewal date, seat usage against the plan's limit, the customer portal button, and the grid with the bought plan marked and unbuyable |
| `past_due` | An amber chip and a line saying a payment failed and the card can be updated in the portal. Access is not cut off, because Stripe is still retrying |
| Cancelling | "Access ends" rather than "Renews", plus a line saying the subscription stops at the end of the period |
| Over the seat limit | A warning line under the seat count, which is where a soft limit shows itself |
| Back from Stripe | `?checkout=success` or `?checkout=canceled` opens the page with the matching sentence. Neither is trusted: the subscription appears when Stripe's event arrives |

Reading is open to every member of the organization, because the plan explains what the whole organization can do. The checkout buttons, the portal link, and the "refresh from Stripe" action are drawn only for `admin` or `billing`, read from the compiled `roles.generated.ts` through `permissions.ts`, which is the same authority the endpoints enforce.

The refresh action is `POST .../reconcile`. It exists on the screen because the one thing a customer does after paying is come back and look, and an event that has not landed yet is the one moment the page can be wrong.

## What this module deliberately leaves out

- **Everything about money.** No card form, no plan-change screen, no invoice list, no tax handling, no dunning email. Stripe Checkout takes the purchase and the customer portal takes every change after it, including plan changes, cancellations, and payment methods. What this framework keeps is the projection it has to authorize from.
- **Proration policy.** Seat changes ride Stripe's default (`create_prorations`). Anything else is a setting on the account.
- **Notifying a customer whose card failed.** `past_due` reaches the row and the screen says so, but nobody is emailed. The event to hang that on is `invoice.payment_failed`, and the surface it needs is notification rather than billing.
- **Time-based usage limits.** Bullet Train's usage trackers meter verbs over a window ("one publish per three days"). Anubis meters counts. A window needs a tracker table and a cycling schedule, which is a larger feature than a limit and is not obviously worth it.
- **A periodic reconciliation sweep.** `Reconciler::reconcile` is written to be called from a schedule; there are no recurring schedules yet. See [jobs.md](jobs.md#roadmap).
- **Multiple live subscriptions, add-ons, and metered prices.** One organization, one subscription, one line item. A customer holding two live subscriptions at Stripe fails the job with a message saying so, because that is a decision a person has to make.
