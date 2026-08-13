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
| Request timeout | 30 seconds, innermost |

The request id is outermost so every later layer logs under it. The timeout is innermost so the response it produces still leaves with an id and the security headers on it.

### Request ids

An inbound `x-request-id` is overwritten rather than honored. The id is the server's own correlation handle, and a caller that could choose it could make two unrelated requests share one line of the log. Handlers read it with `Extension<RequestId>` when they want to name it in an event of their own.

Error bodies stay generic (`{"message": "Something went wrong on our side."}`), so the id is the whole bridge between a user's report and the log line that explains it. Ask for it in your support form.

### Request logging

One `INFO` event per completed request, `WARN` for a `5xx`, carrying the status and the duration, inside a span carrying the method, the path, and the id. Filtering follows `RUST_LOG` like the rest of [telemetry](architecture.md#backend): `debug` by default in development, `info` elsewhere.

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
| `Content-Security-Policy` | `frame-ancestors 'none'` | The same rule in the header that superseded `X-Frame-Options`; both ship, because browsers still disagree about which they honor |

The CSP is deliberately one directive. A real content policy for the SPA needs `script-src` and `style-src` tied to the hashes or nonces of a particular Vite build, which is a build-pipeline change rather than a header change: the server would have to learn what the bundler emitted. A guessed `default-src` would either break the application or be so permissive it proves nothing. `frame-ancestors` is the part that is honest today; the rest is tracked as follow-up work.

**HSTS** (`Strict-Transport-Security: max-age=31536000; includeSubDomains`) is sent only in production, and only when the request arrived over https or `APP_URL` is an https URL. Both conditions matter. A browser that receives HSTS for `localhost` refuses plain http to `localhost` for a year, across every project on that machine, and the only cure is clearing browser state by hand: a development environment broken by a production header. In production, either the deployment terminates TLS (so `APP_URL` says https) or a proxy names the hop it accepted in `x-forwarded-proto`.

Preload is deliberately absent. Submitting a domain to the preload list is close to irreversible and is the operator's decision, not the framework's.

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
| `TRUSTED_PROXY_HEADER` | unset | Forwarding header naming the client address; see [rate limiting](api.md#rate-limiting) |
| `SPA_DIR` | unset | Directory of built frontend assets; see [architecture](architecture.md#deployment) |

The timeout, the drain window, and the readiness timeout are constants rather than variables. They are properties of the deployment shape the framework targets, and a knob per timeout is a knob nobody tunes correctly and everybody has to understand.

## Behind a proxy

A deployment that terminates TLS somewhere else should forward two things: `x-forwarded-proto`, so HSTS applies, and whatever header carries the client address, named in `TRUSTED_PROXY_HEADER` so the rate limiter charges the right client. Without the second one, every request is charged to the proxy.
