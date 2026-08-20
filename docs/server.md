# The server

Every Anubis application boots through one call:

```rust
anubis::server::serve(app, pool, &config).await?;
```

`main` composes routers; the framework owns everything that turns a router into a server people can deploy. That split is deliberate: production behavior nobody has to remember to add is the only kind every application actually has.

`serve` mounts the probes, applies the middleware stack, binds `HOST:PORT`, and serves until the process is asked to stop. `anubis::server::harden` is the same stack without the accept loop, for tests and for applications that host the router elsewhere. `anubis::server::serve_with_shutdown` takes the listener and a shutdown future, which is how an application drives the server and a background [job worker](jobs.md) from one signal.

## The middleware stack

Layers, outermost first. The order is the design:

| Layer | Effect |
|---|---|
| Request id | A fresh UUID per request, in the log span, returned as `x-request-id` |
| Tracing | One event per completed request, inside a span naming the method, path, and id |
| Security headers | The response policy below |
| CORS | Only when `CORS_ALLOWED_ORIGINS` names origins; otherwise absent entirely |
| Compression | Brotli or gzip, when the client accepts one |
| Request timeout | 30 seconds, innermost |

The request id is outermost so every later layer logs under it. The timeout is innermost so the response it produces still leaves with an id and the security headers on it.

### Request ids

An inbound `x-request-id` is overwritten rather than honored. The id is the server's own correlation handle, and a caller that could choose it could make two unrelated requests share one line of the log. Handlers read it with `Extension<RequestId>` when they want to name it in an event of their own.

Error bodies stay generic (`{"message": "Something went wrong on our side."}`), so the id is the whole bridge between a user's report and the log line that explains it. Ask for it in your support form.

### Request logging

One `INFO` event per completed request, `WARN` for a `5xx`, carrying the status and the duration, inside a span carrying the method, the path, and the id. Filtering follows `RUST_LOG` like the rest of [telemetry](architecture.md#backend): `debug` by default in development, `info` elsewhere.

### Compression

Every response is compressed when the client says it accepts one: brotli where it is offered, gzip otherwise, and the plain bytes for a client that offers neither. Text compresses by three to four times, and on a cold page load the bundle and the stylesheet are most of what the browser waits for.

The choice is dynamic compression rather than precompressed files on disk. Precompressing hashed assets at build time is tempting, since the work is paid once and brotli can take its time, but it reaches only files. The JSON the API answers with is most of what a running application sends, and no build step can precompress that. One layer that compresses everything is one decision instead of two, one place to reason about `vary`, and no second artifact per file to go stale. It costs a few milliseconds of CPU on a response the browser then caches for a year. An application that measures a need for precompressed assets can still ship them: a response that arrives already encoded passes through the layer untouched.

The layer sits below the security headers and CORS, so a compressed response leaves with the same headers and the same request id as any other, and above the timeout, so it covers every route including the single-page-application fallback.

What it leaves alone is as deliberate as what it compresses:

| Left alone | Why |
|---|---|
| Bodies under 32 bytes | The headers cost more than the body saves |
| Images, and anything already encoded | Compressed twice is larger, not smaller |
| `text/event-stream` | A stream must flush per event, not per buffer |
| Websocket upgrades | A `101` carries no body at all, so the socket upgrades untouched |

Compressed responses carry `vary: accept-encoding`, so a shared cache never hands brotli to a client that asked for none. `cache-control` is untouched: a hashed asset keeps its year.

### Timeouts

30 seconds per request, answered as `503 Service Unavailable` in the standard error shape. The value is a backstop against a request that will never finish, not a latency budget: an avatar upload decodes and re-encodes an image, and the file handling that follows will be slower still. Lower it below the slowest legitimate handler and a slow success becomes a failure.

`503` rather than `408`, because `408` says the *client* was too slow to send its request, which is the opposite of what happened. `503` says this server could not answer in time and the caller may retry, which is exactly the situation. Dropping the handler's future is what a timeout means in async Rust: the work stops at its next await point and any open transaction rolls back when its connection returns to the pool.

### Graceful shutdown

`SIGTERM`, the signal an orchestrator sends before it kills a container, and ctrl-c both start a drain: the listener stops accepting immediately and in-flight requests get 25 seconds to finish, after which the process exits anyway. Twenty-five seconds fits inside Kubernetes' default 30-second `terminationGracePeriodSeconds`, so the process exits on its own terms instead of being killed mid-response. A request that started just before the signal and wants the full 30-second timeout is therefore cut short, which is the intended trade: by then the traffic gate has already stopped sending work, and a deploy that waits on one straggler is a deploy that hangs.

Windows has no `SIGTERM`, so ctrl-c is the whole story there.

## Liveness and readiness

Two endpoints, because an orchestrator acts on the two answers differently.

| Route | Question | Answer |
|---|---|---|
| `GET /healthz` | Is this process alive? | `200 {"status":"ok"}`, always, with no I/O |
| `GET /readyz` | Can this instance serve traffic? | `200 {"status":"ready"}`, or `503` when the database pool cannot produce a connection within 2 seconds |

Point the restart policy at `/healthz` and the load balancer's traffic gate at `/readyz`. That way a database blip drains traffic from an instance and puts it back when the database returns, instead of restarting every instance at once, which is what a liveness probe that checks the database produces. Readiness checks out a pooled connection and returns it; the pool validates a recycled connection before handing it over, so a connection in hand means the database answered. The `503` body names no dependency, because the probe is unauthenticated; the log line beside it names the cause for the operator who needs it.

Both routes are in the framework's route manifest (`anubis routes`), and a test walks that manifest against the composed routers, so a probe cannot be renamed without the build noticing.

## Security headers

Every response, in every environment:

| Header | Value | What it buys |
|---|---|---|
| `X-Content-Type-Options` | `nosniff` | A browser never second-guesses a `Content-Type`, so an uploaded file cannot be coaxed into executing as script |
| `Referrer-Policy` | `strict-origin-when-cross-origin` | A path carrying an invitation or reset token never leaves in a `Referer` to another site |
| `X-Frame-Options` | `DENY` | No framing, so clickjacking has nothing to hang an overlay on |
| `Content-Security-Policy` | the policy below | What the page may load, and from where |

**HSTS** (`Strict-Transport-Security: max-age=31536000; includeSubDomains`) is sent only in production, and only when the request arrived over https or `APP_URL` is an https URL. Both conditions matter. A browser that receives HSTS for `localhost` refuses plain http to `localhost` for a year, across every project on that machine, and the only cure is clearing browser state by hand: a development environment broken by a production header. In production, either the deployment terminates TLS (so `APP_URL` says https) or a proxy names the hop it accepted in `x-forwarded-proto`.

Preload is deliberately absent. Submitting a domain to the preload list is close to irreversible and is the operator's decision, not the framework's.

## The content security policy

One policy, rendered once at startup and sent with every response:

```
default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline';
img-src 'self' data: blob:; font-src 'self'; connect-src 'self' wss://app.example.com;
base-uri 'self'; form-action 'self'; frame-ancestors 'none'
```

It is derived from what the Vite build emits and what the running application loads, not from what a policy generator suggests:

| Directive | Value | Why |
|---|---|---|
| `default-src` | `'none'` | The base case is refusal, so a resource type nobody thought about is denied rather than inherited from a permissive default |
| `script-src` | `'self'` | The build emits no inline script at all: `index.html` carries one hashed module and one stylesheet, both same-origin, and the lazy chunks are same-origin imports. Nothing in the bundle compiles code at runtime, so there is no `'unsafe-eval'` either |
| `style-src` | `'self' 'unsafe-inline'` | See below |
| `img-src` | `'self' data: blob:` | Avatars and the TOTP QR code are served by this binary. The avatar picker previews the chosen file through `URL.createObjectURL`, which is a `blob:` URL. `data:` rides along because it is how a canvas or an inline SVG hands a browser an image, and because it names no origin, so configuration could not add it later |
| `font-src` | `'self'` | The build self-hosts every face; nothing reaches a font CDN |
| `connect-src` | `'self'` and the websocket origin of `APP_URL` | Every fetch goes to this server's own API, and the realtime channel is a websocket to it. CSP level 3 has `'self'` cover `ws:` on the same host, but browsers implemented that late, and a realtime channel that dies silently in one of them is the worst kind of bug |
| `base-uri` | `'self'` | An injected `<base>` repoints every relative URL on the page, the ones the SPA fetches with included |
| `form-action` | `'self'` | A form may only post back here. The directive does not fall back to `default-src`, so leaving it out would allow every destination |
| `frame-ancestors` | `'none'` | The modern spelling of `X-Frame-Options`; both ship, because browsers still disagree about which they honor |

`frame-src` and `media-src` name nothing, because `default-src 'none'` already refuses them and the frontend embeds neither.

### Why `'unsafe-inline'` is in `style-src`

The component libraries write stylesheets into the document while they run. React Aria adds a `touch-action` rule for every pressable element and an `overscroll-behavior` rule while a modal holds the scroll; Motion inserts one to hold a leaving element in place while it animates out. Some of those honor a nonce and some, including a second copy of React Aria's press handling in the same bundle, set none at all.

A nonce would therefore leave the unnonced ones broken: a policy that reports success while quietly removing behavior. Hashes cannot cover them either, since the rule text is computed from an element's measured position at the moment it leaves.

This is measured rather than assumed: intersect a nonce-only `style-src` over the running application and both injections are refused as `style-src-elem`, React Aria's stylesheet never applying and Motion's positioning silently going missing, because Motion guards on the sheet it was denied.

It is also the standard concession, and a small one. `style-src` is not a code execution boundary; `script-src 'self'` with no inline script and no `'unsafe-eval'` is where the protection lives, and that half is intact.

### Extending it

An application that adds an analytics endpoint, an error ingest, or an image CDN names those sources in `CSP_ALLOWED_SOURCES`, written the way CSP itself is, one group per directive:

```sh
CSP_ALLOWED_SOURCES="script-src https://plausible.io; connect-src https://plausible.io"
```

The sources join the framework's rather than replacing them, so the example above sends `script-src 'self' https://plausible.io`. Seven directives take sources: `script-src`, `style-src`, `img-src`, `font-src`, `connect-src`, `frame-src`, and `media-src`. A source must name a host, optionally wildcarded one label deep (`https://*.example.com`, which CSP does match, unlike CORS).

What is deliberately impossible from an environment variable: widening `default-src`, `base-uri`, `form-action`, or `frame-ancestors`, whose whole value is that they name nothing, and adding `'unsafe-eval'`, a bare scheme like `https:`, or any other keyword, which would turn a configuration typo into an execution gate. Everything is validated at startup, so a rejected value stops the boot instead of disappearing into a policy nobody reads until a widget is blank.

### The one response that carries its own

A handler that sets a `Content-Security-Policy` keeps it: the layer fills the header in, it does not overwrite. Exactly one response in the framework does that, the API reference at `/api/v1/docs`, which renders through Scalar from a CDN and would otherwise be a blank page. Its policy widens `script-src` to that CDN and leaves everything that protects the deployment in place: no framing, no plugins, no form posting elsewhere, and no `'unsafe-eval'`.

### In development

Vite serves the SPA in development and sends no policy of its own; the backend answers only API calls there. A policy on a JSON response restricts nothing, because a policy governs the document that fetched the resource rather than the resource itself, so `yarn dev` behaves exactly as it did. The policy becomes real the moment one binary serves both halves, which is what `SPA_DIR` turns on.

That is the one divergence to know about: an inline `<script>` pasted into `index.html`, or a widget pulled from a CDN, works in `yarn dev` and is refused in production. Run the application the way it deploys before believing a third-party snippet works:

```sh
yarn workspace anubis-starter-frontend build
SPA_DIR=starter/frontend/dist cargo run -p anubis-starter
```

The end-to-end suite runs against that shape too, which is how a change to the policy is proven:

```sh
E2E_BASE_URL=http://localhost:3000 yarn workspace anubis-starter-frontend test:e2e
```

## CORS

The default is no CORS headers at all, which is the strictest posture a browser understands: a cross-origin read is refused without the server saying anything. That is right for the shape Anubis ships, where the same binary serves the SPA and the API and every browser call is same-origin.

`CORS_ALLOWED_ORIGINS` opts in with a comma-separated list of exact origins:

```sh
CORS_ALLOWED_ORIGINS=https://app.example.com,https://admin.example.com
```

Each entry must be a bare `scheme://host[:port]`: no path, no query, no credentials, and no wildcard. Entries are validated and normalized at startup, so a typo fails the boot rather than the first cross-origin call. `https://*.example.com` is rejected rather than stored, because CORS has no notion of subdomain matching and a rule that can never match is worse than an error.

Allowed requests may carry `Authorization` and `Content-Type`, and use the standard methods. Preflights are cached for ten minutes.

**Credentials are never allowed.** The cross-origin consumer this exists for is the public `/api/v1` surface, which authenticates with a bearer token the caller attaches deliberately. Session cookies stay same-origin, where `SameSite=Lax` already keeps them. That also removes the classic footgun in one stroke: there is no combination of settings here that pairs credentials with a permissive origin, because there are neither credentials nor a wildcard.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `HOST` | `127.0.0.1` | Address the server binds to |
| `PORT` | `3000` | Port the server binds to |
| `APP_URL` | `http://<host>:<port>` | Public base URL; an https value is one of the two triggers for HSTS |
| `CORS_ALLOWED_ORIGINS` | unset | Comma-separated exact origins allowed to call the API from a browser |
| `CSP_ALLOWED_SOURCES` | unset | Sources added to the content security policy, per directive |
| `TRUSTED_PROXY_HEADER` | unset | Forwarding header naming the client address; see [rate limiting](api.md#rate-limiting) |
| `SPA_DIR` | unset | Directory of built frontend assets; see [architecture](architecture.md#deployment) |

The timeout, the drain window, and the readiness timeout are constants rather than variables. They are properties of the deployment shape the framework targets, and a knob per timeout is a knob nobody tunes correctly and everybody has to understand.

## Behind a proxy

A deployment that terminates TLS somewhere else should forward two things: `x-forwarded-proto`, so HSTS applies, and whatever header carries the client address, named in `TRUSTED_PROXY_HEADER` so the rate limiter charges the right client. Without the second one, every request is charged to the proxy.
