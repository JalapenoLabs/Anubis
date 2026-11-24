# Realtime channels

The server publishes, browsers listen. `anubis::realtime::Channels` is the one service events are published through, and a browser receives them over Server-Sent Events: one session-authenticated stream at `GET /realtime` carrying every channel the page listens to, plus whatever streams the application serves from its own endpoints, such as a game server's log tail. Every message flows from server to browser, and every write goes over REST, which is the traffic Server-Sent Events are made for: a stream is an ordinary long-lived `GET`, so it passes through proxies and tunnels without upgrade handling, and it resumes natively.

Events carry payloads, typically the changed record in the shape `/api/v1` answers with, so a client can write the change straight into its cache instead of refetching. Every event has an id, and a client that drops and reconnects names the last one it heard and receives what it missed. Redis enters only when an application runs more than one instance, and even then it holds nothing durable.

## Publishing

```rust
use anubis::realtime::ChannelName;

channels
    .publish(
        &ChannelName::team(team.id, "projects"),
        "created",
        &project_view,
    )
    .await?;
```

`Channels` is cheap to clone and holds an `Arc` inside, so handlers, background jobs, and the stream endpoints all share one instance. Build it once at startup with `Channels::from_config(&config)`, or with `Channels::builder()` to give a topic its own replay capacity (below), and clone it wherever it is needed.

The event name is a short verb the client branches on. The payload is any serializable value; publish the same view the REST API answers with, so the client has one shape to apply. `publish` answers the event's id. An event renders to at most 64 KiB, payload and all, because every event is kept for replay; anything larger belongs behind a REST read the event points at.

Publishing is not transactional. A publish that happens inside a transaction is delivered even if that transaction later rolls back, so publish after the commit, or from a background job the commit enqueued. Work that must not be lost belongs in the [job queue](jobs.md), which is transactional by design.

## The namespace

Channel names are structured, never free-form:

```
team:{team_id}:{topic}
user:{user_id}:{topic}
resource:{resource_id}:{topic}
```

The audience in the name is the authorization rule. A team channel admits that team's members, a user channel admits that one user, on every device they are signed in on, and a resource channel admits whoever the application's own endpoint for that record admits. That is the whole rule set, which is why there is no registry of channel permissions to keep in step with the code.

A topic is 1 to 64 characters of lowercase ASCII letters, digits, `_`, `-`, or `.`. Uppercase is excluded because two names differing only in case look identical to a person and route differently. The colon is excluded because it separates the parts of a name. `ChannelName::team`, `ChannelName::user`, and `ChannelName::resource` panic on an invalid topic, which is right for the literals that appear at call sites; names arriving from a browser are parsed with `str::parse` and refused rather than trusted.

Topics are the application's to choose. Name them after the resource whose changes they carry, in the plural, matching the API path that reads it: `projects`, `projects.comments`, `invoices`. One channel per resource collection, not per record, keeps the channels a page carries bounded by what it shows rather than by how much data exists. Resource channels are the exception, because they are about one record: `resource:{server_id}:logs`.

### Authorization

Each channel a stream asks for is authorized on its own, against its name, when the stream opens. Team channels are checked in Postgres, in one query for all of them, through the same rule the `TeamMember` guard applies to team routes: a claimed membership admits, and a suspension at the organization or the sub-tenant above denies. User channels need no query at all. `GET /realtime` refuses every resource channel, because nothing in its name says who may hear it.

A team the user does not belong to, a team that does not exist, a team the user is suspended from, another user's channel, and a resource channel are all refused identically, with `code: "not_found"` and the same message. This is the stream's form of the `404` the ownership-chain guards answer, and it exists for the same reason: a browser must not be able to learn that a team exists by being told it may not listen to it.

A stream authorizes its channels when it opens, and lives five minutes before the server ends it and the client resumes. So a session signed out elsewhere, or a member removed from a team, stops hearing within minutes, at the cost of one reconnect per tab per five minutes that the client makes invisibly.

A replay is authorized with its stream: the channel is checked when the stream opens, and the replay is that channel's kept events after the cursor. A cursor is not bound to a session, and every `live` id shows its epoch, so a subscriber can name an early cursor and hear what the channel still keeps from before it could listen, at most the channel's capacity. That is accepted rather than guarded: it is what `backfill` does on purpose, and the rule that makes it safe is the one this namespace already rests on, that a channel's payloads are what any current member may see.

Role permissions are deliberately not consulted. A team channel carries the fact that something changed and the payload the publisher chose, so membership is the right granularity, and a publisher that would send something only some members may see should split the topic, or serve it as a resource channel from an endpoint that checks the role.

### Extending the namespace

Adding an audience means adding the authorization rule that goes with it, in `ChannelName::audience` and in the endpoint's authorization. Anything that cannot be authorized from the name alone is a resource channel, served by the application's own endpoint. Organization-scoped channels are the obvious next audience and would follow the team rule exactly, through `organization_memberships`.

## Ids and replay

Every publish takes the next number of one sequence shared by every channel, so ids order events across channels as well as within one. That is what lets a stream carrying several channels resume from a single id. An id renders as `{epoch}-{sequence}`, such as `3f2b9c0d1e4a5b6c-42`, and clients treat it as opaque. The epoch names one run of the sequence: in process it is minted when the service starts, and under Redis it is stored beside the sequence and the `run_id` of the Redis server that holds them, and changes whenever that server is a different run. A Redis that restarts, whether it comes back empty or from an older snapshot or append-only file, and a replica promoted in its place, all have a new `run_id`, so a sequence that went backwards never mints again an id a client already holds.

Every event is kept in its channel's buffer, whether or not anyone is listening. A channel keeps its last 64 events, and the deployment keeps 64 MiB of events across every channel, letting go of whole channels, least recently published first, past that. A topic whose events are the data itself usually wants more of its own:

```rust
let channels = anubis::realtime::Channels::builder()
    // The last thousand lines of every server's log.
    .retain("logs", 1_000)
    // Fifteen minutes of one sample a second.
    .retain("resources", 900)
    .replay_budget(512 * 1024 * 1024)
    .build(&config)
    .await?;
```

Retention is per topic whatever the audience, so `retain("logs", 1_000)` applies to `resource:{id}:logs` for every id. Under Redis the publishing instance's numbers are the ones applied, so every instance builds the service the same way, which it does by running the same code.

A reconnecting client sends the id of the last event it heard as `Last-Event-ID`, and the stream opens with every later event on its channels, in sequence order, before anything live. **A replay is exact, or the channel opens with a reset.** The log remembers what it let go: each buffer keeps a floor, the newest event it dropped to stay within capacity, and the log keeps a watermark, the newest event of any channel it dropped whole. A resume from before either is answered for that channel with a `reset` frame, and the client reads the channel's state again. The rules:

| A channel asked for | Opens with |
|---|---|
| With no `Last-Event-ID`, or named in `fresh` | `reset`, so the client reads its state once the stream is live |
| After an id, nothing dropped since | Every kept event after the id, possibly none |
| After an id, its buffer dropped an event since | `reset` |
| After an id, its whole buffer was let go since | `reset` |
| After an id from another epoch, from past the head, or unreadable | `reset` |

A stream served with `backfill()` (below) follows every `reset` with everything the channel still keeps, and opens a first connection with that and no reset at all, because its events are the data itself.

The first row is not a failure: a channel that starts listening opens with a reset because anything the client fetched before the stream was live may already be behind, and refetching once the stream is live is the only way not to miss what happened in between. A view that loads with a REST fetch and listens with `useChannel` therefore revalidates once when its channel goes live.

`fresh` exists because one cursor cannot speak for a channel that was not on the connection it came from: events on that channel before the cursor may never have reached the client, and events after it may predate what the view just fetched. When a page starts listening to a new channel, the client opens a new stream that resumes the channels it already had and names the new one in `fresh`.

## The wire protocol

### Opening a stream

```
GET /realtime?channels=team:{id}:projects,user:{id}:notifications&fresh=user:{id}:notifications
Last-Event-ID: 3f2b9c0d1e4a5b6c-42
```

The session cookie authenticates the request. `channels` names every channel the stream should carry, comma-separated, at most 64, each in the spelling the server renders: lowercase, UUIDs included, since every frame names its channel that way and a listener is found by that name. `fresh` names those among them that do not continue `Last-Event-ID`; without a `Last-Event-ID` every channel is fresh and `fresh` changes nothing. A name given twice is carried once.

| Answer | When |
|---|---|
| `200`, `text/event-stream` | The stream is open; refusals of single channels are frames inside it |
| `401` | No session |
| `403` `password_change_required` | The account must replace its temporary password first |
| `400` | No channels, or more than 64 |
| `429`, with `Retry-After` | The account already holds 32 streams, or 512 channels across them, on this instance; the client waits and asks again |
| `503`, or `500` | The stream could not open right now (the database or Redis was unreachable, or a query failed); the client retries on its backoff |

### Frames

Every frame is one Server-Sent Event whose `data` is a JSON object with a `type` discriminator, so a reader parses one shape whatever it is. Events and the `live` checkpoint also carry the SSE `id` field, which is where `Last-Event-ID` comes from.

| Frame | `id` | Meaning |
|---|---|---|
| `{"type": "event", "channel": ..., "event": ..., "payload": ...}` | yes | One published event |
| `{"type": "reset", "channel": ...}` | no | What the client holds for the channel may be stale: read it again |
| `{"type": "error", "channel": ..., "code": ..., "message": ...}` | no | The stream will not carry the channel |
| `{"type": "live"}` | yes | Everything before this was delivered, or reset |

A stream opens with its refusals, then its resets, then its replay, then `live`, whose id is the head of the sequence when the stream opened. A client that hears nothing else still holds a cursor from that moment, so a quiet stream that drops resumes from when it opened rather than from nowhere. Events follow as they are published.

Refusal codes:

| Code | Meaning |
|---|---|
| `invalid_channel` | Not a channel name in the spelling the server renders |
| `not_found` | The channel does not exist, or is not the subscriber's to hear |
| `too_many_channels` | The process is holding as many channels as it may |

### How a stream ends

A stream ends with a clean close, and the client reconnects at once with its cursor, missing nothing. It ends after its lifetime, five minutes plus up to a minute chosen at random so the tabs a deploy reconnected together do not keep reconnecting together; when the server starts to drain; when it falls 256 events behind, because a stalled connection is cut rather than buffered without limit; and when the delivery path under it broke, such as a lost Redis subscriber connection. In every case the replay log is what makes the end invisible. An expired or draining stream never sends again, even if events are queued for it, and its channels are released on schedule even when its client has stopped reading, so a stalled connection cannot keep hearing, or keep holding, past its lifetime.

While nothing happens, the stream sends a heartbeat comment every twenty seconds, inside every idle timeout a stream is likely to cross: nginx closes a quiet proxied connection after sixty seconds by default and Cloudflare after a hundred.

### Proxies, tunnels, and HTTP/2

A stream answers with `Cache-Control: no-cache, no-transform` and `X-Accel-Buffering: no`, so no cache keeps it, no proxy compresses it into a window, and nginx and the proxies that copied its convention pass each event through as it is written. The server's own compression layer leaves `text/event-stream` alone for the same reason. Cloudflare tunnels carry streams without configuration.

Over HTTP/1.1 a browser opens at most six connections to one origin, and each open stream holds one: `/realtime` and an application stream or two leave room for the page's requests, and a page with many application streams does not. Over HTTP/2 every stream and request shares one connection and the limit does not apply, so a deployment serving several streams per page should terminate HTTP/2 at its edge, which every common proxy and load balancer does.

## Why a fetch rather than EventSource

The framework's client reads streams with `fetch` and a spec-compliant parser rather than the browser's `EventSource`, because `EventSource` sends `Last-Event-ID` only on its own reconnects. A page that starts listening to a new channel opens a new stream, and that stream has to resume the old one, which `EventSource` cannot do. `EventSource` also never says why a connection failed, so a signed-out tab could never tell a `401` from a network blip and stop. The protocol stays plain Server-Sent Events: any `EventSource` can read a stream, it just resumes only on its own reconnects.

## The browser client

`@jalapenolabs/anubis` ships the client, a provider, and two hooks.

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

  useChannel(`team:${teamId}:projects`, (message) => {
    if (message.type === 'reset') {
      void mutate()
      return
    }
    void mutate((page) => applyProjectEvent(page, message), { revalidate: false })
  })

  return ...
}
```

One client is one stream however many channels the page listens to. It opens with the first listener, follows the set as listeners come and go, and closes when the last one leaves. Changes that happen together, such as a page swapping its views in one render, are gathered into one new stream, and the new stream resumes where the old one stopped. Callers see none of that: they hold a disposer and hear their channel until they call it, which `useChannel` does on unmount. Pass `null` as the channel while the name is not known yet, such as before the current team has loaded.

A listener hears a `RealtimeMessage`, an event or a reset. A listener that only refetches treats both alike. One that writes payloads into its cache has to branch, and the compiler holds it to that, because a reset carries no payload. Since a replay is exact or a reset, writing payloads is safe: the cache is never left behind by an event it did not hear.

A stream the server ends reconnects at once; one that fails reconnects on a capped backoff, half a second doubling to ten. One that says nothing at all for 45 seconds, past two heartbeats, is taken for a connection that vanished without closing and reconnects the same way. A `4xx` answer is final until the set of channels changes, because the server will refuse the same request again, except `408`, `425`, `426`, and `429`, which wait and ask again, honoring `Retry-After`; `426` is what an instance still running a release from before event streams answers during a rolling deploy. A channel the server refused never counts as live on the cursor, so if it is admitted on a later connection it opens fresh, with the reset that covers whatever it missed. A listener that throws is logged and the stream carries on.

### Application streams

`useEventStream(url, listener)` opens one of the application's own streams for as long as the component is mounted, with the same resume, the same frames, and the same listener shape. `EventStream` is the class beneath both, for code outside React.

```tsx
useEventStream(`/api/v1/servers/${serverId}/logs`, (message) => {
  if (message.type === 'reset') {
    void refetchOlderLines()
    return
  }
  appendLine(message.payload)
})
```

## Serving a stream of your own

An application streams from its own endpoint with two pieces: the `Subscriber` extractor and `Channels::stream`.

```rust
use anubis::http::ApiError;
use anubis::realtime::{ChannelName, Channels, EventStream, Subscriber};
use axum::Extension;
use axum::extract::Path;
use uuid::Uuid;

/// `GET /api/v1/servers/{server_id}/logs`
async fn logs(
    subscriber: Subscriber,
    Path(server_id): Path<Uuid>,
    Extension(channels): Extension<Channels>,
) -> Result<EventStream, ApiError> {
    servers::authorize_view(subscriber.user(), server_id).await?;
    let stream = channels
        .stream(subscriber.backfill(), [ChannelName::resource(server_id, "logs")])
        .await?;
    Ok(stream)
}
```

`Subscriber` is where the session check lives: it requires a signed-in session exactly as `CurrentUser` does, so a stream can never be mounted without one, and it reads `Last-Event-ID` and the server's drain signal. Authorizing the channels is the handler's, against `subscriber.user()`, which is why any audience may be passed to `stream`, and resource channels above all. The stream then carries everything a `/realtime` stream does: the replay, the reset, heartbeats, its lifetime, and its end on a drain.

`backfill()` changes what a first connection opens with: instead of a reset, every event the channel still keeps, so a log tail opens with its recent lines and a live graph opens full instead of blank. A reconnection resumes from its cursor, and when the buffer can no longer vouch for the cursor it opens with a reset followed by everything still kept, so the tail clears and fills again rather than going blank.

Publishing to the stream is publishing to its channel, from wherever the data arrives:

```rust
channels
    .publish(&ChannelName::resource(server_id, "logs"), "line", &line)
    .await?;
```

### The request timeout

A stream needs no untimed declaration. The 30-second [request timeout](server.md#timeouts) clocks the handler, and a stream's handler answers as soon as the stream is open; the body that follows is never timed, which a test in `anubis::server` pins. So a stream lives as long as its client listens, while its opening (the session check, the authorization, the replay read) stays bounded like any other request.

### Draining

`anubis::server::serve` tells every request when the server starts to drain, and every stream ends at that moment rather than holding the drain for its whole 25-second window. Its client reconnects, to whichever instance answers next, and resumes.

## Backends

The backend is chosen by configuration, never by code. The API, the frames, and the authorization are identical either way.

| `REDIS_URL` | The sequence, the replay log, and the fanout live | A publish reaches | A client resumes on |
|---|---|---|---|
| unset (default) | In this process | The streams on this instance | This instance |
| set | In Redis | The streams on every instance | Any instance |

**In process** is one lock over the sequence, the buffers, and the registry of open streams. A publish mints its id, keeps the event, and queues it for every stream carrying its channel, all under the lock, and a stream opening registers its queue and reads its replay under the same lock, so every event lands in its replay or on its queue and never in neither. This is the whole implementation for a single-instance deployment, which is the shape the framework targets out of the box: no Redis, no extra process, no configuration.

**Redis** moves the sequence and the buffers into Redis and fans events out through pub/sub. A publish is one Lua script that mints the next id, keeps the event in its channel's buffer, trims and evicts exactly as the in-process log does, and publishes it, all atomically, so the order Redis delivers events in is the order of the sequence. A single background pump per process holds the subscriber connection, follows the channels this process's streams carry, and dispatches what arrives into the same registry. The pump subscribes per channel rather than to a pattern, so an instance receives only the channels its own streams carry, and it gathers every subscription change already waiting into one `SUBSCRIBE` and one `UNSUBSCRIBE`, so a burst of tabs reconnecting after a deploy costs a round trip or two rather than one per channel. A stream opening waits for Redis to confirm its subscriptions before it reads its replay with a second script, so an event published in between is on its queue, where the stream skips it if the replay already carried it. That script reads each buffer from its tail, a page at a time, and stops at the first event the client already heard, so a resume near the head reads a page rather than the whole buffer.

A publish therefore reaches the publishing instance the same way it reaches every other one, through Redis. Each stream hears each event exactly once and in sequence order, which a local shortcut would quietly break.

Every key lives under `{anubis:realtime}:`: the log hash (`epoch`, `run_id`, `sequence`, `bytes`, `watermark`), the recency sorted set, the floors and sizes hashes, and one list per channel's buffer. The braces are a Redis Cluster hash tag, so the keys a script touches share one slot. Nothing expires on its own: the byte budget bounds the buffers, and evicting through it is what keeps the watermark true. Run Redis with a `maxmemory-policy` that never evicts keys without a TTL (`noeviction`, or any `volatile-*` policy), because a key Redis evicted behind the framework's back is an event it can no longer vouch for.

Losing Redis costs live delivery until it returns, never data. When the subscriber connection drops, every stream on that instance ends, because whatever was published while it was down never reached their queues, and their clients resume from the replay log once Redis answers. A Redis that restarts, empty or from an older point, starts a new epoch and lets go of everything kept, because both scripts compare the server's `run_id` with the one the log was written under; every client that resumes afterwards opens with resets and refetches. Publishes attempted while Redis is away fail and are not queued for retry.

### Running Redis locally

Development needs none of this, so the compose file keeps Redis behind a profile and the default `docker compose up` stays Postgres-only:

```sh
docker compose -f starter/compose.yaml --profile realtime up -d --wait
REDIS_URL=redis://localhost:63790 cargo run -p anubis-starter
```

There is no volume behind it. The replay buffers are a convenience for clients that reconnect, and a Redis that comes back empty is a Redis that works: its clients refetch once.

## Limits

Everything a stream holds is bounded, because channel names arrive from browsers. The per-account share is what keeps one account from filling the process's table for everyone else: it would take thirty-two accounts holding their whole share to do it, and accounts are themselves bounded by the registration limits.

| Limit | Value | What happens at it |
|---|---|---|
| Channels per stream | 64 | The request is refused with `400` |
| Streams per account, per process | 32 | The request is refused with `429` and `Retry-After` |
| Channels per account, per process | 512, counted across its streams | The request is refused with `429` and `Retry-After` |
| Channels per process | 16,384 | The channel is refused with `too_many_channels` |
| Events queued for one stream | 256 | The stream ends, and its client resumes from replay |
| Events kept per channel | 64, or the topic's own | The oldest go, and a resume from before them resets |
| Bytes kept per deployment | 64 MiB, or the configured budget | Whole channels go, least recently published first |
| One event | 64 KiB | The publish is refused |
| One stream's life | 5 to 6 minutes | The stream ends, and its client resumes |

Backpressure stops at the stream. A slow client falls behind on its own queue and is cut; it never slows a publisher and never grows a buffer on the server without limit.

## Roadmap

Tracked in GitHub issues under M5:

- Per-topic role checks, for applications that want a team channel narrower than membership without serving it as a resource channel.
- Organization-scoped channels, following the team rule through `organization_memberships`.
- Presence: who else is on this page, built on the same channels.
- A scaffolded default: `anubis scaffold` publishing create, update, and delete events for the models it generates, and wiring the generated React pages to apply them.
