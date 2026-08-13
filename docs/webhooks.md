# Outgoing webhooks

A team subscribes an HTTPS endpoint to a set of event types, and every matching domain event arrives there as a signed POST carrying **the same serialized shape the REST API answers with**. This is Bullet Train's outgoing webhooks, rebuilt on the Postgres job queue: one serializer, three consumers (the account UI, `/api/v1`, and the webhook), so a column `anubis scaffold field` adds reaches subscribers with no second declaration.

Nothing about it is opt-in for the application developer. Scaffolded models emit from the functions their account and `/api/v1` handlers share, so a record created through the browser and one created through a bearer token produce identical events.

## The subscription model

| Table | What it holds |
|---|---|
| `webhook_endpoints` | A team's subscription: the URL, the event types, whether it is live, and the signing secret |
| `webhook_deliveries` | One event's journey to one endpoint: the payload, the status, the attempt count, the last response code, and the last error |

An endpoint belongs to a team, exactly as a platform application does, and the deliveries cascade with it. An empty event list receives nothing, and a paused endpoint (`active = false`) keeps its history and receives nothing new.

**The signing secret is encrypted, not hashed.** This is the one place the framework's token discipline does not apply, and the reason is arithmetic: signing a request body means recomputing an HMAC over it at send time, which a one-way hash cannot do. So the secret is sealed with `anubis::auth::secret_box` (AES-256-GCM under `ANUBIS_SECRET_KEY`), the same mechanism a TOTP seed uses. A stolen database yields nothing without the key. The plaintext is returned exactly once, in the create response, and never leaves the server again; a team that loses it deletes the endpoint and creates another. There is no rotation endpoint, because rotating in place would silently break every receiver until its operator noticed.

## Event types

An event type is `<model>.<action>` in snake case, the model singular:

```
project.created
project.updated
project.destroyed
applied_tag.destroyed
```

The actions are `created`, `updated`, and `destroyed`, matching the framework's own `Action::Destroy` so the permission a write needs and the event it produces are spelled the same way. The **catalog is the application's own models**, so the framework validates the shape rather than a list it cannot know: `anubis::webhooks::is_event_type` checks the half the framework does define, and a subscription to `project.create` or `Project.created` is refused where a person typed it rather than sitting there receiving nothing forever.

## Emitting

`anubis::webhooks::emit` is the whole producer surface:

```rust
anubis::webhooks::emit(connection, team_id, "project.created", &view).await?;
```

It finds the team's active endpoints that want the event, writes one `pending` delivery per endpoint, and enqueues one delivery job per row, all through the caller's own connection. That is the point of keeping the queue in Postgres: a generated handler wraps its write, its association reconciliation, and its emission in one transaction, so a rollback sends nothing and a commit never loses its event. [Background jobs](jobs.md) states the guarantee in full.

It returns a query error rather than an error type of its own, because the caller is already inside a Diesel transaction and the failure should join the rollback of the write it belongs to. A payload that will not serialize surfaces as `SerializationError`.

Emitting is cheap and unconditional: a team that has subscribed nothing costs one indexed query and returns `0`.

## Delivering

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

## Verifying a signature

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

### Node

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

### Rust

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

## Security posture

**Transport.** An endpoint must be `https` in production. Plain `http` is accepted outside production only, because a developer testing against `localhost` has no certificate and no attacker; the rule is the environment's, so an endpoint subscribed in development does not become a plaintext request when the same database is promoted. The URL is re-checked at send time, not only at subscribe time.

**Credentials in the URL** are refused, since they would be sent to whatever the host resolves to.

**Server-side request forgery.** A delivery is a request this server makes on a user's instruction, which is SSRF by construction. What is bounded today is the scheme, the absence of embedded credentials, the ten-second timeout, and the refusal to follow redirects. What is **not** bounded is a hostname that resolves to a private address, or one that resolves differently on the second lookup than on the first (DNS rebinding). Closing that needs resolution and connection to happen under one policy, and it is on the roadmap; an application on a network where reaching an internal address is dangerous should egress through a proxy that enforces the boundary.

**Authorization.** The management endpoints are team-scoped through the `TeamMember` guard and require the admin role, for the same reason platform applications do: a subscription decides where a team's records are sent. Another tenant's endpoint answers `404`, identically to one that does not exist.

**What a payload carries** is exactly what the team's own API answers with for that record. There is no wider view: a webhook cannot leak a column `/api/v1` does not serve.

## The endpoints

Team-scoped, under `/developers`, beside the platform applications described in [the REST API](api.md#authentication).

| Method | Path | Effect |
|---|---|---|
| GET | `/developers/teams/{team_id}/webhook-endpoints` | List the team's subscriptions |
| POST | `/developers/teams/{team_id}/webhook-endpoints` | Subscribe an endpoint; returns the signing secret once |
| PATCH | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}` | Change the URL, the events, the description, or pause it |
| DELETE | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}` | Unsubscribe, deliveries included |
| GET | `/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries` | The delivery log, newest first, page/limit |
| POST | `.../deliveries/{delivery_id}/redeliver` | Queue a fresh attempt of one delivery |

## The screen

The starter owns a Developers page per team, at `/teams/{teamId}/developers`. It is not at `/developers`, which the backend reserves for the API itself: a client route under a reserved prefix answers JSON on a cold load.

The page hosts one card today. It lists the team's endpoints with their events and an active switch, subscribes new ones, and shows the signing secret in a modal that has to be dismissed, because there is no second chance to read it. Each row opens a delivery log showing the event, the status, the attempt count, the response code, the last error, and a redeliver button. A member without the admin role sees the list and no form, which mirrors the `403` the server would answer anyway.

## Roadmap

Tracked in GitHub issues under M5:

- **Incoming webhooks**: `anubis scaffold webhook <name>` generates a receiving endpoint, its signature verification, and its tests.
- **DNS-rebinding-grade SSRF protection**: resolve and connect under one policy, with a configurable deny list for private ranges.
- **Delivery retention**: deliveries accumulate forever today. Pruning belongs with the recurring-schedule work in [jobs.md](jobs.md#roadmap).
- **A per-endpoint event picker** in the UI, once the application's model catalog is exposed to the frontend; the field takes typed event types today and validates their shape.
