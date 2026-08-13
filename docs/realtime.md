# Realtime channels

The server publishes, browsers subscribe. `anubis::realtime::Channels` is the one service events are published through, and every browser receives them over a single websocket at `GET /realtime`. Redis enters only when an application runs more than one instance, and even then it is a fanout and never a store.

Realtime is a notification bus, not a log. An event tells a client that something changed; the REST API is where it reads what it changed to. Nothing here is durable, nothing is replayed, and a client that was disconnected refetches rather than catching up. That is what keeps Redis optional and keeps the system of record in one place.

## Publishing

```rust
use anubis::realtime::ChannelName;
use serde_json::json;

channels
    .publish(
        &ChannelName::team(team.id, "projects"),
        "created",
        json!({ "id": project.id, "name": project.name }),
    )
    .await?;
```

`Channels` is cheap to clone and holds an `Arc` inside, so handlers, background jobs, and the websocket route all share one instance. Build it once at startup with `Channels::from_config(&config)` and clone it wherever it is needed.

The event name is a short verb the client branches on. The payload is any serializable value. Publishing to a channel nobody is listening to succeeds and costs one map lookup: a publisher never has to know who is connected.

Publishing is not transactional. A publish that happens inside a transaction is delivered even if that transaction later rolls back, so publish after the commit, or from a background job the commit enqueued. Work that must not be lost belongs in the [job queue](jobs.md), which is transactional by design.

## The namespace

Channel names are structured, never free-form:

```
team:{team_id}:{topic}
user:{user_id}:{topic}
```

The audience in the name is the authorization rule. A team channel admits that team's members, and a user channel admits that one user, on every device they are signed in on. That is the whole rule set, which is why there is no registry of channel permissions to keep in step with the code.

A topic is 1 to 64 characters of lowercase ASCII letters, digits, `_`, `-`, or `.`. Uppercase is excluded because two names differing only in case look identical to a person and route differently. The colon is excluded because it separates the parts of a name. `ChannelName::team` and `ChannelName::user` panic on an invalid topic, which is right for the literals that appear at call sites; names arriving from a browser are parsed with `str::parse` and refused rather than trusted.

Topics are the application's to choose. Name them after the resource whose changes they carry, in the plural, matching the API path that reads it: `projects`, `projects.comments`, `invoices`. One channel per resource collection, not per record, keeps subscription counts bounded by what a page shows rather than by how much data exists.

### Authorization

A subscribe request is authorized on its own, against the name it asks for, when it arrives. Membership is checked in Postgres for team channels; user channels need no query at all.

A team the subscriber does not belong to and a team that does not exist are refused identically, with `code: "not_found"` and the same message. This is the websocket's form of the `404` the ownership-chain guards answer, and it exists for the same reason: a browser must not be able to learn that a team exists by being told it may not listen to it.

Role permissions are deliberately not consulted. A channel carries the fact that something changed and the payload the publisher chose, so membership is the right granularity, and a publisher that would send something only some members may see should split the topic instead. Per-topic role checks are tracked as future work.

### Extending the namespace

Adding an audience means adding the authorization rule that goes with it, in `ChannelName::audience` and in the socket's `access` function. Anything that cannot be authorized from the name alone does not belong in the namespace. Organization-scoped channels are the obvious next audience and would follow the team rule exactly, through `organization_memberships`.

## The wire protocol

One websocket carries every channel a browser is interested in. Frames are JSON objects with a `type` discriminator, in both directions. The session cookie authenticates the upgrade, so an unauthenticated request is refused `401` before any websocket exists.

What a browser sends:

```json
{"type": "subscribe",   "channel": "team:0b2f8c1e-...:projects"}
{"type": "unsubscribe", "channel": "team:0b2f8c1e-...:projects"}
```

Every client frame gets exactly one reply. What the server sends:

| Frame | Meaning |
|---|---|
| `{"type": "subscribed", "channel": ...}` | The subscription is live; events follow |
| `{"type": "unsubscribed", "channel": ...}` | The subscription is gone |
| `{"type": "event", "channel": ..., "event": ..., "payload": ...}` | One published event |
| `{"type": "error", "channel": ..., "code": ..., "message": ...}` | A request was refused, or a subscription degraded |

Error codes:

| Code | Meaning |
|---|---|
| `invalid_frame` | Not JSON, no known type, or not a valid channel name |
| `not_found` | The channel does not exist, or is not the subscriber's to hear |
| `too_many_channels` | The socket, or the process, is holding as many channels as it may |
| `lagged` | The connection fell behind and the server dropped events for it |
| `internal` | The server could not answer; the client may retry |

Unknown fields on a client frame are ignored, so a newer client may carry extra keys without an older server refusing its frames. Subscribing twice to one channel is idempotent, and so is unsubscribing from a channel the socket never held.

The server pings every 20 seconds and closes a socket that has sent nothing at all for 60 seconds, so a connection that vanished without a close frame releases its subscriptions instead of holding them forever. Incoming frames are capped at 4 KiB, which is thirty times the size of the largest frame a client has reason to send.

## The browser client

`@jalapenolabs/anubis` ships the client and one hook.

```tsx
import { RealtimeClient, RealtimeProvider, useChannel } from '@jalapenolabs/anubis'

const realtime = new RealtimeClient()

function App() {
  return <RealtimeProvider client={realtime}>
    <Projects />
  </RealtimeProvider>
}

function Projects() {
  const { data, mutate } = useSWR('projects', listProjects)

  useChannel(`team:${teamId}:projects`, (event) => {
    void mutate()
  })

  return ...
}
```

One client is one websocket however many channels the page watches. It connects on the first subscription, retries a dropped connection on a capped exponential backoff (500 ms doubling to a ceiling of 10 seconds), and resubscribes every channel that still has a listener as soon as the new connection opens. Callers see none of that: they hold a disposer and receive events until they call it, which `useChannel` does on unmount.

Pass `null` as the channel while the name is not known yet, such as before the current team has loaded. Nothing is subscribed until it is.

Because events are not replayed, refetching is the right reaction to most of them. Applying payloads straight into local state is fine for a view that can tolerate being briefly wrong, and wrong is exactly what it will be after a reconnect that spanned a write.

## Fanout backends

The backend is chosen by configuration, never by code. The API, the frames, and the authorization are identical either way.

| `REDIS_URL` | Backend | A publish reaches |
|---|---|---|
| unset (default) | In process | The browsers connected to this instance |
| set | Redis pub/sub | The browsers connected to every instance |

**In process** is a registry of tokio broadcast channels. A channel exists only while somebody is listening to it, so the registry is bounded by live interest rather than by how many channel names have ever been used. This is the whole implementation for a single-instance deployment, which is the shape the framework targets out of the box: no Redis, no extra process, no configuration.

**Redis** adds two things to that registry and changes nothing else. Publishes go out over a reconnecting command connection, prefixed into the framework's own namespace (`anubis:realtime:`). A single background pump holds the subscriber connection, follows the channels this process is listening to, and dispatches everything Redis sends it into the same registry. The pump subscribes per channel rather than to a pattern, so an instance receives only the channels its own browsers asked for.

A publish therefore reaches the publishing instance the same way it reaches every other one, through Redis. Local subscribers see each event exactly once and in the same order as remote ones, which a local shortcut would quietly break.

Losing Redis costs live delivery until it returns, never data. The pump reconnects on a backoff capped at five seconds and re-declares every subscription on the new connection, because the server that held the old ones is not the one answering now. Publishes attempted while Redis is away fail and are not queued for retry; the clients that missed them refetch.

### Running Redis locally

Development needs none of this, so the compose file keeps Redis behind a profile and the default `docker compose up` stays Postgres-only:

```sh
docker compose -f starter/compose.yaml --profile realtime up -d --wait
REDIS_URL=redis://localhost:63790 cargo run -p anubis-starter
```

There is no volume behind it. A fanout has nothing worth persisting, and a Redis that comes back empty is a Redis that works.

## Limits

Everything the socket holds is bounded, because channel names arrive from browsers.

| Limit | Value | What happens at it |
|---|---|---|
| Channels per socket | 64 | The subscribe is refused with `too_many_channels` |
| Channels per process | 16,384 | The subscribe is refused with `too_many_channels` |
| Events a subscriber may fall behind | 64 | The oldest are dropped and the client is sent `lagged` |
| Queued frames per socket | 64 | The subscription task waits, and the client eventually lags |

Backpressure stops at the socket. A slow browser slows its own delivery, falls behind, and is told so; it never slows a publisher and never grows a buffer on the server without limit.

## Roadmap

Tracked in GitHub issues under M5:

- Per-topic role checks, for applications that want a channel narrower than team membership.
- Organization-scoped channels, following the team rule through `organization_memberships`.
- Presence: who else is on this page, built on the same channels.
- A scaffolded default: `anubis scaffold` publishing create, update, and delete events for the models it generates, and wiring the generated React pages to revalidate on them.
