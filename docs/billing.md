# Billing

Anubis bills organizations for subscription plans through Stripe. Plans live in one configuration file, money lives at Stripe, and the application keeps only the projection it has to authorize from.

This mirrors Bullet Train's billing experience (`bullet_train-billing` plus `bullet_train-billing-stripe`) with one file of products and prices, Stripe Checkout for the purchase, the Stripe Billing customer portal for every change after it, and per-plan usage limits. What differs is where things attach: Bullet Train subscribes a Team, Anubis subscribes an **Organization**, because the organization is the tenant that owns teams and pays for them. See [tenancy.md](tenancy.md).

## Status

The module lands in three slabs. This document marks what is built and what is not, so nothing here reads as a promise the code does not keep.

| Slab | What it covers | State |
|---|---|---|
| 1 | Plans in config, the Stripe client, the data model, checkout and portal endpoints | Built |
| 2 | Incoming Stripe webhooks and the subscription lifecycle | Not built |
| 3 | Limit enforcement and the billing UI | Not built |

Until slab 2 lands, a completed checkout charges the customer at Stripe and this application still shows the free plan, because nothing writes the subscription row yet. Run slab 1 against Stripe's test mode only.

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
| `STRIPE_API_BASE` | `https://api.stripe.com` | Where Stripe's API lives; for tests and Stripe-compatible mocks only |

`STRIPE_SECRET_KEY` is the switch, and it follows the same posture as `SMTP_URL`:

- **Unset** (the development default): every organization is on the free plan, `GET` keeps answering with `billing_enabled: false`, and checkout and portal answer `503` naming the variable. The UI hides what cannot work.
- **Production without it**: the application boots and warns at startup. An application that does not charge yet should not be blocked on billing configuration.
- **Malformed**: a publishable key (`pk_...`) is refused by name at boot, because it is the key people reach for first and every call made with it would fail at Stripe.

The key is a bearer credential for an account that moves money, so it is never echoed in an error, a log line, or a `Debug` rendering.

`anubis doctor` validates `config/billing.yml` with the parser the server boots with, and reports its absence as "billing is off for this application" rather than as a problem.

## Endpoints

Mounted by the application under `/billing`, and listed by `anubis routes`.

| Route | Guard | Effect |
|---|---|---|
| `GET /billing/organizations/{organization_id}` | org member | The plan in force, the subscription behind it, and whether billing is configured |
| `POST /billing/organizations/{organization_id}/checkout` | org `admin` or `billing` | Opens a Stripe Checkout session, answers with its URL |
| `POST /billing/organizations/{organization_id}/portal` | org `admin` or `billing` | Opens the Stripe customer portal, answers with its URL |

Reading is open to every member of the organization because the plan and its limits explain what the whole organization can do. Spending money takes the `admin` role or the `billing` role, which the starter's `roles.yml` ships described as "can manage billing and subscriptions". A member of another organization gets `404` on all three, the same as a member of none: the `OrganizationMember` guard never reveals that an organization exists.

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

## Why a hand-written Stripe client

`anubis::billing::stripe::Client` is roughly 200 lines over the `reqwest` client the framework already carries. The alternative considered was the `async-stripe` crate.

- **Surface area**: the whole integration is three calls today and five when subscription events land. Every one is a form-encoded POST answering flat JSON. A generated SDK models the entire Stripe API to serve three calls.
- **Pure-Rust TLS is mandatory** here, and both options can satisfy it, but `reqwest` with rustls is already in the tree for outgoing webhook delivery, so this adds no dependency and no second TLS stack. `cargo tree -i openssl` matches nothing, and CI keeps it that way.
- **Compile time**: this workspace compiles in seconds and that is worth protecting. `async-stripe` is one of the larger crates in the ecosystem, and its API surface changes with Stripe's.
- **The escape hatch stays open**: the client is one module behind a small surface. An application that needs more of Stripe than the framework uses can depend on `async-stripe` itself without fighting anything here.

Conventions the client holds to:

- Requests are `application/x-www-form-urlencoded` with Stripe's nested parameter syntax (`metadata[organization_id]`, `line_items[0][price]`).
- Responses are parsed into the few fields the framework reads. Stripe adds fields constantly, and a struct insisting on all of them would break on their schedule rather than ours.
- Creating a customer carries an `Idempotency-Key` derived from the organization, so two checkouts started at the same instant converge on one customer instead of racing to create two. The store is then conditional on the column still being null, so neither request can overwrite the other.
- Checkout sessions carry `client_reference_id`, `metadata[organization_id]`, `metadata[plan_key]`, and the same two values under `subscription_data[metadata]`, so every event that follows identifies the tenant without a lookup table of Stripe ids.
- No `Stripe-Version` header is sent, so calls run on the account's default API version. Pinning one is a single constant when a deployment wants its upgrades to be deliberate.

## Setting Stripe up

1. Create the products and prices in the Stripe dashboard (start in test mode: <https://dashboard.stripe.com/test/products>).
2. Copy each price id into `config/billing.yml`.
3. Put the secret key in `.env` as `STRIPE_SECRET_KEY` (<https://dashboard.stripe.com/test/apikeys>).
4. Activate the customer portal and choose which products it may switch between (<https://dashboard.stripe.com/test/settings/billing/portal>). List only plans this application defines, or a plan change will produce an event naming a price no plan sells.
5. Repeat all of it in live mode before launch, including a live `STRIPE_SECRET_KEY`.

Webhook configuration belongs to slab 2 and is not needed to open a checkout session.

## Testing

`anubis/tests/billing_flow.rs` is the narrative, and it runs against a **mock Stripe**: a small axum server serving the three endpoints the framework calls, pointed at with `STRIPE_API_BASE`, exactly as `oauth_flow.rs` serves a mock OpenID Connect provider. Nothing in the code under test is special-cased for the test, so the assertions cover the real request bodies: the price the plan names, the metadata the next slab reads, the idempotency key, and the `Authorization` header.

It uses `TestDatabase`, so each narrative owns a database and can count rows; see [testing.md](testing.md).

## What slab 2 needs from slab 1

The interfaces are in place and stable:

- `PlanSet::find_by_price_id(price_id) -> Option<(&Plan, Interval)>` resolves the plan a Stripe subscription bought. Validation guarantees at most one plan sells any price.
- `SubscriptionStatus::parse` reads Stripe's status words; `Interval::parse` reads both `monthly` and Stripe's `month`.
- `Subscription::current_for_organization` is the read every consumer uses, and the unique index makes it single-valued.
- The checkout session's metadata carries `organization_id` and `plan_key` onto the subscription itself.
- `anubis::webhooks::signature::verify_hmac_sha256` is the provider-agnostic comparison Stripe's `Stripe-Signature: t=<unix>,v1=<hex>` scheme ends in; the signed message is `<t>.<body>`. See [webhooks.md](webhooks.md).

Open questions slab 2 has to settle:

- **Where the receiver lives.** `anubis scaffold webhook Stripe` generates an application-owned receiver, but a framework-owned subscription lifecycle needs a framework-mounted endpoint. The likely answer is a framework receiver at `/webhooks/stripe` reusing the store-first, process-after discipline (see [webhooks.md](webhooks.md)), with `STRIPE_WEBHOOK_SECRET` as the second billing variable.
- **Which events are handled**: at minimum `checkout.session.completed`, `customer.subscription.created`, `customer.subscription.updated`, `customer.subscription.deleted`, and probably `invoice.payment_failed` for notification.
- **Retrieving rather than trusting.** An event's body can be stale by the time it is processed; the honest path is to re-read the subscription from Stripe by id, which needs a fifth client call (`GET /v1/subscriptions/{id}`).
- **Reconciliation.** A missed event leaves the projection wrong forever. A periodic job that re-reads every live subscription is the usual answer.

## What slab 3 needs from slab 1

- `Plan::limits()` and `Plan::limit(name)` are the whole limit surface, already validated.
- `PlanSet` is a plain, ordered, serializable structure, so a `anubis billing generate-ts` emitter renders the plan catalog into the SPA the way `anubis roles generate-ts` renders permissions. The frontend then needs no endpoint to draw a pricing page.
- `GET /billing/organizations/{id}` already answers with the plan, the subscription, and `billing_enabled`, which is what a billing screen renders.
- The client route to add is `/organizations/:organizationId/billing`, which is where Stripe returns the browser.

Open questions slab 3 has to settle:

- **Hard versus soft limits.** Bullet Train marks each limit `hard` (the form is disabled) or `soft` (the user is warned). The current schema is `name: count`; adding enforcement means either a nested shape or a naming convention, and the choice is slab 3's.
- **Per-seat pricing.** `quantity` exists on the line item and on the row, but checkout sends `1`. Charging per membership means deciding what counts as a seat and keeping the quantity current as members join and leave.
- **Time-based limits.** Bullet Train's usage trackers meter verbs over a window. That is a larger feature than a count and may not be worth it.
