# Architecture

Anubis is a Rust-first SaaS framework modeled on Bullet Train's developer experience: teams-first multi-tenancy, roles and permissions, an auto-generated versioned REST API, and a Super Scaffolding style code generator. The stack is a Rust backend with a React SPA frontend.

Everything Bullet Train does at runtime through Rails reflection, Anubis does at codegen time. After a scaffold runs, `cargo check` and `tsc` prove the whole stack links end to end. That compile-time guarantee is the core advantage of this stack.

## Backend

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (pinned toolchain) | World-class, idiomatic Rust throughout |
| Async runtime | tokio | The entire backend is async |
| Web framework | Axum | Maintained by the tokio team; sits on hyper + tower |
| Middleware | tower / tower-http | Sessions, auth guards, tracing, CORS, compression |
| ORM | Diesel + diesel-async | Fully compile-time typed queries against a generated `schema.rs`; no SQL strings, no runtime query surprises |
| Database | PostgreSQL (required, pinned version) | System of record for everything, including sessions and jobs |
| Cache + realtime | Redis (optional) | Pub/sub fanout for realtime channels and hot caching; never the system of record |
| Background jobs | Postgres-backed queue | Job enqueue commits in the same transaction as the domain write that caused it |
| Passwords | argon2id | |
| Secrets at rest | AES-256-GCM (`aes-gcm`, pure Rust) | For secrets the app must read back, such as TOTP seeds; everything else is hashed. Keyed by `ANUBIS_SECRET_KEY` (base64, 32 bytes), required in production, with a public development fallback that warns at startup |
| OAuth / SSO | OpenID Connect (`openidconnect` crate) | Providers added via `anubis scaffold oauth <provider>` |
| Observability | tracing | Structured events with named properties |
| Errors | Canonical error structs in the framework library; `eyre`/`anyhow` style results allowed in generated application code | Follows the Rust guidelines in force at Jalapeno Labs |

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
| Unit tests | Vitest |
| E2E tests | Playwright |

Route guards preserve where the user was headed. When the auth guard turns a signed-out visitor away, it sends them to `/sign-in?next=<path>`, and the guest guard returns them to that path the moment a session exists. The destination rides in the query string because the flow that needs it most, an invitation link opened from an email, is a cold page load that router state would not survive. Every destination passes through `sanitizeDestination` in the app's `urls.ts`, which accepts root-relative paths only, so the parameter cannot become an open redirect.

## Packaging

One Rust crate, one npm package, one monorepo.

- `anubis/` is a single Cargo package. It is both the framework library and the `anubis` CLI binary (`cargo install anubis` provides the CLI). Optional functionality (billing, webhooks) lives behind additive cargo features, all enabled by default.
- `frontend/` is a single npm package (`@jalapenolabs/anubis`). It ships the app shell (nav, breadcrumbs, team switcher, settings pages), the field component library, the auth pages, and the generated-client runtime. Tree shaking keeps consuming apps lean.
- `starter/` is the template that `anubis new <name>` stamps out. It is deliberately thin: config, composition, and the application's own domain code. Framework behavior lives in the crate and the npm package so upgrades are version bumps, not template merges. The starter doubles as the host app that keeps the scaffolding templates compiling in CI.
- `docs/` holds one document per decision category.

Bullet Train's most-cited long-term cost is merging upstream starter changes after customization. Anubis avoids that cost structurally by keeping the starter thin and shipping everything else as versioned dependencies.

## The contract pipeline

The API contract flows in one direction, from Rust to TypeScript:

1. Handlers and serializers register with utoipa, producing an OpenAPI 3.1 document.
2. `anubis client generate-ts` turns that document into TypeScript types and ky route functions in house style.
3. Application code consumes those functions through SWR hooks.

Scaffolding a model or field regenerates the contract; anything the frontend must update surfaces as a TypeScript compile error.

## Deployment

The target: a production deployment is one static Rust binary serving the API and the built SPA assets, PostgreSQL, and optionally Redis, with small Docker images and versions pinned everywhere. Serving the SPA from the binary is not implemented yet (tracked in GitHub issues, M5), so today the frontend requires its own static host.

## Roadmap

The scaffolder stamps out patterns, so the patterns are hand-built and stabilized first, then automated.

- **M1 Foundation**: monorepo layout, `anubis` crate skeleton, config, errors, tracing, auth (register, login, sessions, email verification, password reset), React shell with auth pages. A new app boots to a logged-in dashboard.
- **M2 Tenancy**: Organizations, Teams, Memberships, Invitations, Roles, the `roles.yml` compiler, ownership-chain guards, org/team switcher UI. See [tenancy.md](tenancy.md).
- **M3 API layer**: `/api/v1` structure, platform applications and bearer tokens, OpenAPI generation, the TypeScript client pipeline. See [api.md](api.md).
- **M4 Scaffolding**: the `anubis` CLI generators, the field component library, `scaffold model` and `scaffold field` end to end with generated tests. See [scaffolding.md](scaffolding.md).
- **M5 Ecosystem**: outgoing and incoming webhooks, background jobs, billing, i18n polish, eject tooling.
