# Webhooks

Webhooks are how an application and the systems around it tell each other that something happened, and Anubis has both halves. **Outgoing** webhooks publish the application's own events to endpoints its teams subscribe. **Incoming** webhooks receive a third party's events at endpoints `anubis scaffold webhook` generates.

The two halves are asymmetric because the questions are. A publisher decides what to send, who may subscribe, and how to prove a message is its own. A receiver decides none of that: it is handed bytes by a machine it does not control, on a schedule it cannot influence, and its whole job is to lose none of them.

## Outgoing: publishing the application's own events

A team subscribes an HTTPS endpoint to a set of event types, and every matching domain event arrives there as a signed POST carrying **the same serialized shape the REST API answers with**. This is Bullet Train's outgoing webhooks, rebuilt on the Postgres job queue: one serializer, three consumers (the account UI, `/api/v1`, and the webhook), so a column `anubis scaffold field` adds reaches subscribers with no second declaration.

Nothing about it is opt-in for the application developer. Scaffolded models emit from the functions their account and `/api/v1` handlers share, so a record created through the browser and one created through a bearer token produce identical events.

### The subscription model

| Table | What it holds |
|---|---|
| `webhook_endpoints` | A team's subscription: the URL, the event types, whether it is live, and the signing secret |
| `webhook_deliveries` | One event's journey to one endpoint: the payload, the status, the attempt count, the last response code, and the last error |

An endpoint belongs to a team, exactly as a platform application does, and the deliveries cascade with it. An empty event list receives nothing, and a paused endpoint (`active = false`) keeps its history and receives nothing new.

**The signing secret is encrypted, not hashed.** This is the one place the framework's token discipline does not apply, and the reason is arithmetic: signing a request body means recomputing an HMAC over it at send time, which a one-way hash cannot do. So the secret is sealed with `anubis::auth::secret_box` (AES-256-GCM under `ANUBIS_SECRET_KEY`), the same mechanism a TOTP seed uses. A stolen database yields nothing without the key. The plaintext is returned exactly once, in the create response, and never leaves the server again; a team that loses it deletes the endpoint and creates another. There is no rotation endpoint, because rotating in place would silently break every receiver until its operator noticed.

### Event types

An event type is `<model>.<action>` in snake case, the model singular:

```
project.created
project.updated
project.destroyed
applied_tag.destroyed
```

The actions are `created`, `updated`, and `destroyed`, matching the framework's own `Action::Destroy` so the permission a write needs and the event it produces are spelled the same way. The **catalog is the application's own models**, so the framework validates the shape rather than a list it cannot know: `anubis::webhooks::is_event_type` checks the half the framework does define, and a subscription to `project.create` or `Project.created` is refused where a person typed it rather than sitting there receiving nothing forever.

### Emitting

`anubis::webhooks::emit` is the whole producer surface:

```rust
anubis::webhooks::emit(connection, team_id, "project.created", &view).await?;
```

It finds the team's active endpoints that want the event, writes one `pending` delivery per endpoint, and enqueues one delivery job per row, all through the caller's own connection. That is the point of keeping the queue in Postgres: a generated handler wraps its write, its association reconciliation, and its emission in one transaction, so a rollback sends nothing and a commit never loses its event. [Background jobs](jobs.md) states the guarantee in full.

It returns a query error rather than an error type of its own, because the caller is already inside a Diesel transaction and the failure should join the rollback of the write it belongs to. A payload that will not serialize surfaces as `SerializationError`.

Emitting is cheap and unconditional: a team that has subscribed nothing costs one indexed query and returns `0`.

### Delivering

The job is `anubis::webhooks::DeliverWebhook`, on its own `webhooks` queue so a customer endpoint that hangs for ten seconds cannot hold up an application's own work. An application registers it on its worker in three lines:

```rust
let deliverer = anubis::webhooks::Deliverer::new(pool.clone(), &config);
let worker = anubis::jobs::Worker::builder(pool.clone())
    .register(move |job: anubis::webhooks::DeliverWebhook| {
        let deliverer = deliverer.clone();
        async move { deliverer.deliver(job).await }
    })
    .build();
```

The starter's `main.rs` does exactly that, and stops the worker after the server has drained, because a request still finishing may yet enqueue work.

Each attempt POSTs the payload with a ten-second timeout and no redirect following (a redirect would carry a body signed for the first host to a second one the team never subscribed). A `2xx` marks the row `delivered`; anything else records the response code and the error and returns a failure, which the queue retries on its own widening backoff (10s, 40s, 90s, 160s, 250s). A delivery that spends all five attempts is marked `dead` and stops.

| Status | Meaning |
|---|---|
| `pending` | Queued, not yet attempted |
| `delivered` | The endpoint answered `2xx` |
| `failed` | The last attempt failed and another is coming |
| `dead` | Out of attempts; nothing will retry it without a person |

Delivery is **at-least-once**, like every job on this queue, so receivers must be idempotent. The `anubis-webhook-id` header is stable across retries of one delivery, which is the key to deduplicate on. A row that is already `delivered` returns without sending, so a redundant job is harmless.

**Redelivering** creates a fresh delivery row from the original's payload and queues it. It does not reset the original: the first attempt's history is what the team is looking at when they press the button, and overwriting it would answer their question by erasing it.

### Verifying a signature

Every delivery carries four headers:

| Header | Value |
|---|---|
| `anubis-webhook-id` | The delivery's id, stable across retries |
| `anubis-webhook-event` | The event type, e.g. `project.created` |
| `anubis-webhook-timestamp` | Unix seconds the request was signed at |
| `anubis-webhook-signature` | `v1=<hex>` |

The signature is an HMAC-SHA256 over the timestamp, a literal `.`, and the exact request body, keyed with the endpoint's signing secret, rendered as lowercase hex:

```
v1 = hex(HMAC-SHA256(secret, "<timestamp>.<body>"))
```

Three rules make verification actually safe:

1. **MAC the raw bytes you read off the wire.** Any JSON library is free to reorder keys or change spacing on a round trip, so a signature over a re-serialization will not match.
2. **Compare in constant time.** A byte-by-byte comparison that returns early leaks the correct signature one byte at a time.
3. **Reject a timestamp far from now.** The timestamp is inside the MAC, so an attacker replaying a captured request cannot move it forward without invalidating the signature. Five minutes is the framework's own window.

A future scheme is published as `v2=` **beside** `v1=` in the same header, so receivers that only know `v1` keep working while they migrate. Split the header on commas and accept any scheme you recognize.

#### Node

```js
import { createHmac, timingSafeEqual } from 'node:crypto'

// The raw body, not a parsed-and-reserialized one.
export function verifyAnubisWebhook(rawBody, headers, secret) {
  const timestamp = Number(headers['anubis-webhook-timestamp'])
  if (!Number.isFinite(timestamp) || Math.abs(Date.now() / 1000 - timestamp) > 300) {
    return false
  }

  const expected = createHmac('sha256', secret)
    .update(`${timestamp}.${rawBody}`)
    .digest()

  return String(headers['anubis-webhook-signature'])
    .split(',')
    .some((candidate) => {
      const hex = candidate.trim().replace(/^v1=/, '')
      const presented = Buffer.from(hex, 'hex')
      return presented.length === expected.length && timingSafeEqual(presented, expected)
    })
}
```

#### Rust

An Anubis application receiving these deliveries has the check already:

```rust
use anubis::webhooks::signature;

let accepted = signature::verify(
    &secret,
    timestamp,
    signature_header,
    raw_body,
    chrono::Utc::now().timestamp(),
);
```

### Security posture

**Transport.** An endpoint must be `https` in production. Plain `http` is accepted outside production only, because a developer testing against `localhost` has no certificate and no attacker; the rule is the environment's, so an endpoint subscribed in development does not become a plaintext request when the same database is promoted. The URL is re-checked at send time, not only at subscribe time.

**Credentials in the URL** are refused, since they would be sent to whatever the host resolves to.

**Server-side request forgery.** A delivery is a request this server makes on a user's instruction, which is SSRF by construction. What is bounded today is the scheme, the absence of embedded credentials, the ten-second timeout, and the refusal to follow redirects. What is **not** bounded is a hostname that resolves to a private address, or one that resolves differently on the second lookup than on the first (DNS rebinding). Closing that needs resolution and connection to happen under one policy, and it is on the roadmap; an application on a network where reaching an internal address is dangerous should egress through a proxy that enforces the boundary. The gap is asserted rather than assumed: `webhook_endpoints_may_still_name_an_address_inside_the_network` in the adversarial suite subscribes a link-local address and expects it to be accepted, so the day the gap closes, the test fails and gets rewritten.

**Authorization.** The management endpoints are team-scoped through the `TeamMember` guard and require the admin role, for the same reason platform applications do: a subscription decides where a team's records are sent. Another tenant's endpoint answers `404`, identically to one that does not exist.

**What a payload carries** is exactly what the team's own API answers with for that record. There is no wider view: a webhook cannot leak a column `/api/v1` does not serve.

### The endpoints

Team-scoped, under `/developers`, beside the platform applications described in [the REST API](api.md#authentication).

| Method | Path | Effect |
|---|---|---|
| GET | `/developers/teams/{team_id}/webhook-endpoints` | List the team's subscriptions |
| POST | `/developers/teams/{team_id}/webhook-endpoints` | Subscribe an endpoint; returns the signing secret once |
| PATCH | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}` | Change the URL, the events, the description, or pause it |
| DELETE | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}` | Unsubscribe, deliveries included |
| GET | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries` | The delivery log, newest first, page/limit |
| POST | `.../deliveries/{delivery_id}/redeliver` | Queue a fresh attempt of one delivery |

### The screen

The starter owns a Developers page per team, at `/teams/{teamId}/developers`. It is not at `/developers`, which the backend reserves for the API itself: a client route under a reserved prefix answers JSON on a cold load.

The page hosts one card today. It lists the team's endpoints with their events and an active switch, subscribes new ones, and shows the signing secret in a modal that has to be dismissed, because there is no second chance to read it. Each row opens a delivery log showing the event, the status, the attempt count, the response code, the last error, and a redeliver button. A member without the admin role sees the list and no form, which mirrors the `403` the server would answer anyway.

## Incoming: receiving a third party's events

```
anubis scaffold webhook Stripe
```

One command generates a whole receiver for one provider: the table its requests are stored in, the unauthenticated endpoint that stores them, the signature check, and the background job that processes them. This is Bullet Train's `super_scaffold:incoming_webhook` rebuilt on the Postgres job queue, and it keeps Bullet Train's central decision: **a received webhook is stored before it is understood.**

The argument is the provider, not the model. `Stripe` gives a `StripeWebhook` model in `backend/src/stripe_webhooks/`, stored in `stripe_webhooks`, received at `/webhooks/stripe`, signed with `STRIPE_WEBHOOK_SECRET`. Write the provider the way it should read in the URL: `github` gives `/webhooks/github`.

**The framework has one receiver of its own**, and it mounts beside these: `POST /webhooks/stripe-billing` takes the Stripe events that keep subscriptions current, because the subscription lifecycle is a framework feature rather than an application's ([billing.md](billing.md)). The paths do not collide, which is the point of the name: an application that scaffolds `Stripe` for its own Connect accounts or one-off payments keeps `/webhooks/stripe`, with its own table, its own job, and its own signing secret. `anubis scaffold webhook StripeBilling` is refused by name, since two routers claiming one path panic at boot.

### Store, then process

The endpoint does two things and no more: it writes the request down, and it queues a job. Both happen in one transaction, so a stored webhook always has work queued for it and a queued job always has a row to read. Then it answers `200`, and everything the payload means is decided afterwards.

That ordering is the whole design, and it follows from what a provider does to a receiver that is slow or fails: it retries, often aggressively, and eventually disables the endpoint. Work done inside the request is work that can time out and be sent again; work done after the `200` is work the queue owns, with its own backoff, its own attempt count, and its own dead-letter table. The endpoint is therefore as fast as one insert and as durable as the database.

| Column | What it holds |
|---|---|
| `payload` | The provider's JSON, exactly as it arrived |
| `headers` | The request headers, minus `authorization` and `cookie` |
| `verified` | Whether the signature checked out |
| `received_at` | When the request arrived, which is the row's creation time |
| `processed_at` | When processing finished, or `NULL` while the row is still waiting |
| `error` | The most recent processing failure |

The table is **not team-owned**, unlike everything `anubis scaffold model` generates. A provider posting an event is not signed in and names no team, and which of the application's records the event belongs to is a question only the payload can answer. So there is no ownership chain, no `roles.yml` entry, and no `/api/v1` surface: nobody browses received webhooks, the application processes them.

The one request refused rather than stored is a body that is not JSON, which the `payload` column cannot hold. Every provider sends JSON, so that is a misconfigured endpoint rather than an event, and a `400` saying so is more useful than a row nothing can read.

### Verifying, and why an unverified request is still stored

A published URL is not a secret, so an endpoint that believes whatever arrives at it believes the internet. The generated `verify_signature` is where that is settled, and it is **the one function the scaffolder cannot finish**, because every provider signs differently.

What ships is the shape they all share: read the signature header, recompute the MAC over the bytes that arrived, and compare in constant time. `anubis::webhooks::signature::verify_hmac_sha256` is that comparison, and it is all the arithmetic any of these schemes need. What differs is the header's spelling and what goes into the MAC:

| Publisher | Header | Signed message |
|---|---|---|
| The generated default | `x-webhook-signature: <hex>` | the exact request body |
| GitHub | `X-Hub-Signature-256: sha256=<hex>` | the exact request body |
| Stripe | `Stripe-Signature: t=<unix>,v1=<hex>` | `<t>.<body>` |
| Another Anubis application | `anubis-webhook-signature: v1=<hex>` | `<timestamp>.<body>` |

Two of those rows need no work at all. An Anubis publisher is verified with `anubis::webhooks::signature::verify`, and the timestamp window and the multi-scheme header come with it. Stripe is verified with `anubis::webhooks::signature::verify_stripe`, which parses the `t=,v1=` header, MACs `<t>.<body>`, accepts any of several `v1` entries so a secret rotation at Stripe is seamless, and refuses a timestamp more than five minutes from now. It lives here rather than in the billing module because the scheme is a scheme: the framework's own billing receiver calls it, and so should an application's Stripe receiver.

Whatever the scheme, **MAC the raw bytes**. Re-serializing the parsed JSON is free to reorder keys and change spacing, and the signature then never matches. The generated handler verifies before anything parses, for exactly that reason.

The answer is recorded in `verified` rather than used to refuse the request, which is deliberate and is what Bullet Train does. (The framework's own billing receiver refuses instead, and stores nothing: it knows Stripe's scheme exactly, so a request that fails is an attacker or a mistyped secret rather than an unfinished function. [billing.md](billing.md) states the trade in full.) Refusing at the edge throws away the one piece of evidence that explains what happened, and a signature check that is subtly wrong then looks exactly like a provider that never called. Storing it means an operator can read the request, compare it with the provider's own delivery log, and fix the check. What an unverified event is *worth* is a different question, and the job answers it: usually nothing, so it returns an error and the row stays as evidence.

A provider with no signing scheme has weaker options, and the generated handler is where they go: an allowlist of source addresses, a secret in a header the provider lets you set, or a secret in the path. All three are worse than an HMAC, and all three beat nothing.

### Processing

The generated job's `act_on` is the second function to fill in, and it is where a stored event becomes something the application did. Everything around it is finished: the row is loaded, an already-processed row returns without acting, a success stamps `processed_at`, and a failure records the message on the row **and** returns the error, so the queue retries on its widening backoff and eventually dead-letters the job.

Delivery is at-least-once, like every job on this queue, so `act_on` may run twice for one webhook. The `processed_at` stamp guards against that, and handling that is not naturally idempotent needs a guard of its own.

Processing runs on its own `incoming_webhooks` queue, so a provider replaying a day of events cannot starve the work a person is waiting on. [Background jobs](jobs.md) states the queue's guarantees in full.

### Routing, and why receivers are not rate limited

Receivers mount under `/webhooks`, through the application's own `webhooks_router` in `backend/src/lib.rs`, deliberately outside `/account` so no guard ever asks a provider for a session it does not have. The framework's billing receiver nests under the same prefix from `main.rs`, and `/webhooks` is one of the SPA's reserved prefixes, so a provider posting to a mistyped path gets a JSON `404` it can act on rather than an HTML page and a `200`.

They carry **no rate limit**. The budgets in `anubis::rate_limit` exist to make credential guessing expensive and are sized for humans; webhooks arrive at machine rates and in bursts, so those budgets would refuse real events, and a refused event is what puts an endpoint on a provider's retry schedule and eventually gets it disabled. What bounds the work is its shape instead: one insert per request, over a body the server already caps.

### What one run generates

- a timestamped migration creating `<provider>_webhooks`, with a partial index on the unprocessed backlog and the shared `set_updated_at()` trigger
- the `diesel::table!` block in `backend/src/schema.rs`
- the module under `backend/src/<provider>_webhooks/`: the model with its store and stamp queries, the endpoint with the signature check, and the job with its registration
- the module declaration, the router mount in `webhooks_router`, and the job registration in `register_jobs`, all in `backend/src/lib.rs`
- the receiver's own integration test in `backend/tests/<provider>_webhooks_flow.rs`

and nothing on the frontend, because a receiver has no UI. The run then prints the two steps only a person can take: registering the endpoint's URL in the provider's console, and setting `<PROVIDER>_WEBHOOK_SECRET`. One endpoint per provider, so a second run for the same one is refused by name.

[Scaffolding](scaffolding.md#anubis-scaffold-webhook-one-provider-one-endpoint) covers the command itself: the template it transforms, the anchors it writes above, and what it refuses.

## Roadmap

Tracked in GitHub issues under M5:

- **DNS-rebinding-grade SSRF protection**: resolve and connect under one policy, with a configurable deny list for private ranges.
- **Retention on both sides**: deliveries and received webhooks accumulate forever today. Pruning belongs with the recurring-schedule work in [jobs.md](jobs.md#roadmap).
- **A per-endpoint event picker** in the UI, once the application's model catalog is exposed to the frontend; the field takes typed event types today and validates their shape.
- **A screen for received webhooks**: the table is read through the database today. An operator's view of what arrived, what was verified, and what failed belongs beside the outgoing delivery log.
- **A provider registry**, the way `anubis scaffold oauth` already has one: a Stripe or GitHub receiver would then arrive with that provider's real header and message shape rather than the generic HMAC default, and `verify_signature` would be a review rather than a task. Stripe's half of that already exists as `signature::verify_stripe`; what is missing is the scaffolder knowing to call it.
