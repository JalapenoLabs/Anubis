# Architecture

Anubis is a Rust-first SaaS framework modeled on Bullet Train's developer experience: teams-first multi-tenancy, roles and permissions, an auto-generated versioned REST API, and a Super Scaffolding style code generator. The stack is a Rust backend with a React SPA frontend.

Everything Bullet Train does at runtime through Rails reflection, Anubis does at codegen time. After a scaffold runs, `cargo check` and `tsc` prove the whole stack links end to end. That compile-time guarantee is the core advantage of this stack.

## Backend

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (pinned toolchain) | World-class, idiomatic Rust throughout |
| Async runtime | tokio | The entire backend is async |
| Web framework | Axum | Maintained by the tokio team; sits on hyper + tower |
| Middleware | tower / tower-http | Sessions, auth guards, per-client rate limits, request ids, tracing, security headers, opt-in CORS. See [server.md](server.md) |
| ORM | Diesel + diesel-async | Fully compile-time typed queries against a generated `schema.rs`; no SQL strings, no runtime query surprises |
| Database | PostgreSQL (required, pinned version) | System of record for everything, including sessions and jobs |
| Realtime channels | One websocket at `/realtime`, session-authenticated | Team- and user-scoped channels, published from handlers and jobs, subscribed by the SPA. See [realtime.md](realtime.md) |
| Cache + realtime fanout | Redis (optional) | Set `REDIS_URL` and a publish reaches every instance's subscribers; unset, channels are served in process. Never the system of record |
| Background jobs | Postgres-backed queue | Enqueue commits in the same transaction as the domain write that caused it. At-least-once, retried on a widening backoff, then dead-lettered. See [jobs.md](jobs.md) |
| Passwords | argon2id | |
| Abuse limits | In-process GCRA, per client and per email recipient | Budgets on the auth endpoints, answering `429` with `Retry-After`. Bounded memory, no Redis, and therefore per instance. See [api.md](api.md#rate-limiting) |
| Secrets at rest | AES-256-GCM (`aes-gcm`, pure Rust) | For secrets the app must read back, such as TOTP seeds; everything else is hashed. Keyed by `ANUBIS_SECRET_KEY` (base64, 32 bytes), required in production, with a public development fallback that warns at startup |
| OAuth / SSO | OpenID Connect (`openidconnect` crate, reqwest + rustls, no native TLS) | Authorization code with PKCE, server-side state and nonce. Google ships; a provider is enabled by two environment variables, and the sign-in page renders whichever `GET /auth/oauth/providers` reports. `anubis scaffold oauth <provider>` prints that setup and writes nothing. See [api.md](api.md#oauth-sign-in) |
| Outgoing email | SMTP via `lettre` (tokio + rustls, no native TLS) | One `Mailer` service with log, test, and SMTP backends, selected by `SMTP_URL`. See [email.md](email.md) |
| Observability | tracing | Structured events with named properties |
| Errors | Canonical error structs in the framework library; `eyre`/`anyhow` style results allowed in generated application code | Follows the Rust guidelines in force at Jalapeno Labs |

### Migrations at boot

An application applies the framework's embedded migrations and then its own, both at startup, so a freshly stamped app migrates itself on first start and a deploy carries its schema with it.

Applying takes a Postgres advisory lock over one fixed key, held for the length of the pass. Two instances booting at the same moment is the ordinary case, not the exotic one, and without the lock they race in Postgres' catalog: the loser fails a `CREATE TABLE` with a unique violation on `pg_type_typname_nsp_index`, which reads like nothing to do with migrations. With it, the second waits and then finds nothing pending. See [testing.md](testing.md#concurrent-migrations).

### Secrets at rest and key rotation

`ANUBIS_SECRET_KEY` is base64 for exactly 32 random bytes. `anubis secret generate` prints one, and `anubis doctor` warns when the variable is unset (the public development key is in use) and fails when it holds something that cannot be a key.

Rotating the key is a one-way door: every value sealed under the old one stops opening, which is what makes stolen ciphertext worthless the moment the key is replaced. In practice that means TOTP enrollments are discarded and those users enroll a second factor again, and any other sealed secret is reissued. Treat a rotation as user communication, not just a deploy.

Rotating without that cost needs a dual-key read path: a second variable holding the previous key, tried when the current one fails, so values re-seal under the new key as they are read. That is future work; today there is one key.

## Frontend

| Concern | Choice |
|---|---|
| Framework | React SPA (client-side rendered), Vite + SWC, TypeScript |
| UI components | HeroUI (pinned to the v2.8 line; the v3 major requires React 19 and a new component API, so migrating is a deliberate decision tracked in GitHub issues) + Jalapeno Labs UI Kit |
| Styling | TailwindCSS, themed via the Jalapeno Labs Brand package |
| State | Redux Toolkit |
| Routing | React Router with a central `UrlTree` |
| Data fetching | ky + SWR |
| Forms | react-hook-form + zod resolvers |
| i18n | i18next, per-model locale files emitted by the scaffolder |
| Unit and component tests | Vitest, with Testing Library over happy-dom. See [testing.md](testing.md#frontend-tests) |
| E2E tests | Playwright |

Route guards preserve where the user was headed. When the auth guard turns a signed-out visitor away, it sends them to `/sign-in?next=<path>`, and the guest guard returns them to that path the moment a session exists. The destination rides in the query string because the flow that needs it most, an invitation link opened from an email, is a cold page load that router state would not survive. Every destination passes through `sanitizeDestination` in the app's `urls.ts`, which accepts root-relative paths only, so the parameter cannot become an open redirect.

## Packaging

One Rust crate, one npm package, one monorepo.

- `anubis/` is a single Cargo package. It is both the framework library and the `anubis` CLI binary (`cargo install anubis` provides the CLI). It declares no cargo features: webhooks, jobs, realtime, and billing are all part of the library, and each one costs nothing until an application mounts it. Features are reserved for the day a subsystem carries a dependency an application should be able to refuse; every feature would otherwise be an additive one that is always on.
- `frontend/` is a single npm package (`@jalapenolabs/anubis`). It ships the app shell (nav, breadcrumbs, team switcher, settings pages), the field component library, the auth pages, and the generated-client runtime. Tree shaking keeps consuming apps lean.
- `starter/` is the template that `anubis new <name>` stamps out. It is deliberately thin: config, composition, and the application's own domain code. Framework behavior lives in the crate and the npm package so upgrades are version bumps, not template merges. The starter doubles as the host app that keeps the scaffolding templates compiling in CI.
- `docs/` holds one document per decision category.

Bullet Train's most-cited long-term cost is merging upstream starter changes after customization. Anubis avoids that cost structurally by keeping the starter thin and shipping everything else as versioned dependencies.

## The contract pipeline

The API contract flows in one direction, from Rust to TypeScript:

1. Handlers and serializers register with utoipa, producing an OpenAPI 3.1 document. The application owns that document: its `lib.rs` declares the version's identity and merges the framework's half and one line per scaffolded model into it.
2. The application's binary exports the merged document (`<binary> openapi`), and `anubis client generate-ts --from <file>` turns it into TypeScript types and ky route functions in house style.
3. Application code consumes those functions through SWR hooks.

Scaffolding a model or field changes the document, so the export and the generation run again; anything the frontend must update surfaces as a TypeScript compile error. CI runs both and fails on a diff, for the framework's own client and for the starter's.

## Deployment

A production deployment is one Rust binary serving the API and the built SPA, PostgreSQL, and optionally Redis, with small Docker images and versions pinned everywhere.

The binary boots through `anubis::server::serve`, which is where production behavior lives: liveness at `/healthz` and readiness at `/readyz`, a request id on every response, one log event per request, a per-request timeout, security headers, opt-in CORS, and a bounded drain on `SIGTERM`. An application's `main` composes routers and calls it once. See [server.md](server.md).

`SPA_DIR` points the binary at the frontend's build output and is the whole switch:

```sh
yarn workspace anubis-starter-frontend build
SPA_DIR=starter/frontend/dist cargo run -p anubis-starter
```

The assets mount as the router's fallback, so the API keeps precedence without a route list: every path a mounted router claims is answered by that router, and everything else resolves to the SPA. Files under `assets/` carry content hashes, so they are served with a one-year `immutable` `Cache-Control`; `index.html`, which names them, is served with `no-cache`, so a deploy is live on the next page load. Any other path serves `index.html` too, which is what makes a cold load of a client-side route work.

Paths under the framework's own prefixes (`/api`, `/auth`, `/billing`, `/developers`, `/realtime`, `/tenancy`, `/users`) are the exception: an unmatched path there answers the API's JSON `404` rather than the SPA, because HTML with a `200` turns a routing mistake into a parse error far from its cause. Applications reserve their own prefixes the same way; the starter reserves `/account`.

Leaving `SPA_DIR` unset serves the API alone, which is both the development default (Vite owns the browser there) and a supported production shape for a frontend hosted on a CDN. Production logs a warning when it is unset. When it is set, the directory is validated at startup, so a deploy that shipped without a build fails immediately instead of at the first page load.

## Roadmap

The scaffolder stamps out patterns, so the patterns are hand-built and stabilized first, then automated.

- **M1 Foundation**: monorepo layout, `anubis` crate skeleton, config, errors, tracing, auth (register, login, sessions, email verification, password reset), React shell with auth pages. A new app boots to a logged-in dashboard.
- **M2 Tenancy**: Organizations, Teams, Memberships, Invitations, Roles, the `roles.yml` compiler, ownership-chain guards, org/team switcher UI. See [tenancy.md](tenancy.md).
- **M3 API layer**: `/api/v1` structure, platform applications and bearer tokens, OpenAPI generation, the TypeScript client pipeline. See [api.md](api.md).
- **M4 Scaffolding**: the `anubis` CLI generators, the field component library, `scaffold model` and `scaffold field` end to end with generated tests. See [scaffolding.md](scaffolding.md).
- **M5 Ecosystem**: outgoing and incoming webhooks, background jobs, realtime channels, billing, i18n polish, eject tooling. The job queue ships (see [jobs.md](jobs.md)), and so do realtime channels (see [realtime.md](realtime.md)) and both halves of webhooks (see [webhooks.md](webhooks.md)). Billing ships its purchase and lifecycle halves: plans in configuration, the Stripe client, subscriptions on the organization, the checkout and portal endpoints, and the Stripe receiver that keeps subscriptions current (see [billing.md](billing.md)); limit enforcement and the billing UI follow.
