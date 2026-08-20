# Anubis

[![CI](https://github.com/JalapenoLabs/Anubis/actions/workflows/ci.yml/badge.svg?branch=develop)](https://github.com/JalapenoLabs/Anubis/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**The open-source SaaS framework for Rust and React.** The developer experience
of [Bullet Train](https://bullettrain.co), rebuilt on a Rust backend and a React
SPA.

Bullet Train proved two things. The same-in-every-SaaS plumbing (teams, roles,
auth, a versioned public API, webhooks, billing) belongs in a framework rather
than in your repository. And a great code generator turns domain modeling into
the highest-leverage activity in application development. Anubis brings both to
teams who want Rust and React instead of Ruby and ERB.

## The pitch

```sh
anubis new acme-crm
cd acme-crm
anubis scaffold model Company Team industry:text_field employees:number_field
```

One command, nineteen artifacts, both ends (paths condensed here; the real run
lists every one):

```
scaffolded Company (owned by a team)

created:
  backend/src/companies/{mod,model,routes}.rs
  backend/tests/companies_flow.rs
  backend/migrations/…_create_companies/{up,down}.sql
  frontend/src/api/routes/companyRoutes.ts
  frontend/src/components/CompanyForm.tsx
  frontend/src/locales/models/companies.en-US.json
  frontend/src/pages/{CompaniesPage,CompanyPage}.tsx
updated:
  backend/src/{schema,lib}.rs
  config/roles.yml
  frontend/src/{roles.generated.ts,i18n.ts,urls.ts,App.tsx}
  frontend/src/components/AppShell.tsx
```

A migration, a Diesel model with its ownership chain, account CRUD handlers,
`/api/v1` handlers with their OpenAPI registrations, permission grants, an
integration test, a typed API module, a form, a list page, a show page,
navigation, routes, and a locale file. Then `anubis scaffold field` adds one
column and carries it through all of the model's own artifacts, from the
migration to the locale file.

**Because both ends are statically typed, a scaffold either compiles end to end
or tells you exactly what to fix.** That is the promise Rails can never make.
Everything Bullet Train does at runtime through Rails reflection, Anubis does at
codegen time, and `cargo check` plus `tsc` are the proof.

## Quick start

```sh
cargo install --git https://github.com/JalapenoLabs/Anubis.git anubis
anubis new acme-crm --license mit
cd acme-crm && yarn install && yarn dev
```

The application is at <http://localhost:5173>, with Postgres in Docker and the
migrations already applied.

**[Getting started](docs/getting-started.md)** is the full path: prerequisites,
your first model, associations, OAuth in one line, the public API, webhooks,
billing, tests, and the deployment image.

**[The nine-minute demo](docs/demo.md)** is the same ground at demo speed, timed
and rehearsed, ready for a screencast.

## The stack

- **Backend**: Rust on tokio, Axum, tower, Diesel with diesel-async, PostgreSQL,
  optional Redis for realtime fanout and caching.
- **Frontend**: React SPA with Vite, TypeScript, HeroUI, TailwindCSS, Redux
  Toolkit, React Router, ky with SWR, react-hook-form with zod, i18next.
- **Contract**: OpenAPI 3.1 generated from the Rust handlers, driving a
  generated, fully typed TypeScript client.
- **Codegen**: `anubis scaffold` produces migrations, models, permissions,
  handlers, API docs, React pages, navigation, locale files, and tests from one
  command, on Bullet Train's living-templates philosophy.
- **Deployment**: one binary serving the API and the built SPA, from a
  three-stage image that installs no system libraries.

## What ships today

Anubis is pre-alpha, and this table is the honest storefront. Bullet Train's
feature categories are on the left.

| Area | Status | What that means |
|---|---|---|
| **Authentication** | Ships | Password (argon2id), passwordless email codes, passkeys (WebAuthn), TOTP second factor with recovery codes, email verification, password reset, session listing and revocation, avatars, account deletion |
| **OAuth / SSO** | Ships | OpenID Connect with PKCE. Google in the registry; a provider is two environment variables, and `anubis scaffold oauth <provider>` prints them |
| **Multi-tenancy** | Ships | User, TeamMembership, Team, Organization, OrganizationMembership, and Invitation. A personal organization and default team at signup. Team and organization settings screens, rosters, and the switcher |
| **Roles and permissions** | Ships | One `config/roles.yml`, compiled to a Rust authorization module and a TypeScript affordances module, drift-gated in CI |
| **Scaffolding** | Ships | `scaffold model`, `field`, `join`, `oauth`, `webhook`. Six field spellings today (`text_field`, `text_area`, `number_field`, `boolean`, `date_field`, and `super_select` for both association shapes). Two levels of ownership |
| **Field components** | Ships | Eighteen React controls in `@jalapenolabs/anubis`, including rich text, a code editor, and file and image pickers. The generator's table is the subset the living templates prove |
| **REST API** | Ships | Versioned `/api/v1`, per-team platform applications with bearer tokens, OpenAPI 3.1, Scalar docs, and a generated TypeScript client, all drift-gated |
| **Outgoing webhooks** | Ships | Per-team subscriptions, HMAC-SHA256 signatures, at-least-once delivery on the job queue, a delivery log with redelivery, and a debugging screen |
| **Incoming webhooks** | Ships | `anubis scaffold webhook <Provider>` generates the table, the store-then-process endpoint, the signature check, the job, and the test |
| **Background jobs** | Ships | A durable Postgres queue. Enqueue rides the caller's transaction, at-least-once delivery, widening retry backoff, then a `dead_jobs` table |
| **Realtime** | Ships | One session-authenticated websocket, team- and user-scoped channels, in-process fanout by default and Redis across instances |
| **Billing** | Ships | Plans in `config/billing.yml`, Stripe Checkout and the customer portal, the subscription projection kept current by Stripe's events, reconciliation, hard and soft limits, per-seat pricing, and the billing screen |
| **Email** | Ships | One `Mailer` with log, test, and SMTP backends, plus optional DKIM signing |
| **Abuse limits** | Ships | Per-client and per-recipient budgets on the auth endpoints, answering `429` with `Retry-After`, and a bounded gate in front of every argon2 computation |
| **Production server** | Ships | Liveness and readiness probes, request ids, request logging, security headers, opt-in CORS, compression, a per-request timeout, and a bounded drain on `SIGTERM` |
| **Eject** | Ships | `anubis eject <Component>` copies a field component into your application, stamps its provenance, and rewires the imports |
| **Testing** | Ships | Unit tests, narrative integration suites against real Postgres, concurrency and adversarial suites, Vitest on both packages, and Playwright end to end |
| **i18n** | Partial | i18next throughout and per-model locale files emitted by the scaffolder. English is the only bundled locale |
| **Three-level ownership** | Deferred | `Task Goal,Project,Team` is refused by name. [The design is written down](docs/scaffolding.md#deferred-a-third-level-of-ownership) |
| **Themes, dark mode, mobile navigation** | Not yet | One Tailwind and HeroUI look, a desktop navbar, no theme engine |
| **Admin panel, impersonation, onboarding wizard** | Not yet | No equivalent of Bullet Train's Avo integration or "become user" |
| **Notifications, conversations, audit log** | Not yet | Tracked for after M5 |
| **Published packages** | Not yet | Applications depend on the framework from git until the crate and the npm package publish |

## Documentation

- [Getting started](docs/getting-started.md) and [the nine-minute demo](docs/demo.md)
- [Architecture](docs/architecture.md)
- [Scaffolding](docs/scaffolding.md)
- [Tenancy, teams, and organizations](docs/tenancy.md)
- [REST API](docs/api.md)
- [The server](docs/server.md)
- [Background jobs](docs/jobs.md)
- [Webhooks](docs/webhooks.md)
- [Realtime channels](docs/realtime.md)
- [Billing](docs/billing.md)
- [Email](docs/email.md)
- [Testing](docs/testing.md)
- [CI](docs/ci.md)

## Status

Pre-alpha, and close to a first release. The architecture is settled and
documented in [docs/](docs/). Milestones M1 Foundation, M2 Tenancy, M3 API
Layer, M3.5 Identity, M4 Scaffolding, and M5 Ecosystem are all closed. M6 Launch
is the work in progress: publishing the packages, hardening the browser policy,
and the documentation you are reading.

## Repository layout

- `anubis/`: the single Cargo package (framework library + `anubis` CLI binary)
- `frontend/`: the single npm package (`@jalapenolabs/anubis`)
- `starter/`: the template stamped by `anubis new`, and the CI host app for the scaffolding templates
- `docs/`: one document per decision category
- `bullet_train/`: Bullet Train reference material (docs, submodule, research report)

## Contributing

`main` is production (stable); `develop` is the working branch. Stakeholders
push to `develop` directly; everyone else opens a pull request into `develop`.
Promotion fast-forwards or merges `develop` into `main`, so `main` is never
ahead of `develop`.

Before touching a living template, run the scaffold proof, which generates a
whole domain into `starter/` and holds the output to the bar hand-written code
is held to:

```sh
docker compose --env-file .env.example -f starter/compose.yaml up -d --wait
DATABASE_URL=postgres://anubis_starter:anubis-starter-dev-password@localhost:54321/anubis_starter_development \
  bash scripts/ci-scaffold-proof.sh
```

Tech debt and future work live in GitHub Issues, grouped by milestone.

## License

MIT. Anubis is a [Jalapeno Labs](https://github.com/JalapenoLabs) project.
