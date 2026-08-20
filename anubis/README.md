# anubis-framework

The Rust half of [Anubis](https://github.com/JalapenoLabs/Anubis), an open-source SaaS framework: the developer experience of [Bullet Train](https://bullettrain.co), rebuilt on a Rust backend and a React SPA frontend.

This crate is both the framework library and the `anubis` CLI binary. Its React counterpart is the [`@jalapenolabs/anubis`](https://www.npmjs.com/package/@jalapenolabs/anubis) npm package, and the two move together: one version, one release.

The crate is `anubis-framework` because an unrelated crate holds `anubis` on crates.io. Everything you type is still `anubis`: the library, the binary, and the dependency itself, through Cargo's dependency renaming.

```toml
[dependencies]
anubis = { package = "anubis-framework", version = "0.1.0" }
```

## What the library gives an application

- **Tenancy**: User, TeamMembership, Team, Organization, with invitations, ownership chains, and a personal org and default team at signup.
- **Authentication**: passwords over argon2, TOTP and passkeys, OpenID Connect sign-in, sessions, and per-endpoint abuse budgets.
- **A versioned REST API**: `anubis::api::v1::router_with`, an OpenAPI 3.1 document the application owns, and platform tokens.
- **Permissions**: one `roles.yml`, compiled to Rust authorization and TypeScript UI affordances.
- **Billing**: plans as configuration, Stripe Checkout and the customer portal, hard and soft limits, per-seat pricing.
- **Background jobs**: a durable Postgres queue that needs no Redis, enqueued on the caller's connection so a job commits with the write that caused it.
- **Webhooks**: outgoing deliveries signed with HMAC-SHA256, and incoming receivers that store first and process afterwards.
- **Realtime**: one session-authenticated websocket, in-process fanout by default, Redis pub/sub when `REDIS_URL` is set.
- **Serving**: `anubis::server::serve`, which owns health checks, request ids, tracing, security headers, CORS, timeouts, and a bounded drain on `SIGTERM`.

## The CLI

```sh
cargo install anubis-framework   # the crate is `anubis-framework`; the binary is `anubis`
anubis new my-app          # stamp a complete application
anubis scaffold model ...  # generate a model end to end: backend, frontend, tests, docs
anubis doctor              # check the environment a deployment needs
```

`anubis new` carries the starter template inside the binary, so stamping is offline and the template always matches the installed version.

## Requirements

PostgreSQL is required; Redis is optional and only ever a cache or a fanout, never durability. The crate builds with no native dependencies: no libpq, no OpenSSL.

## Documentation

Architecture, tenancy, scaffolding, the API, billing, jobs, webhooks, realtime, and testing each have a document in [`docs/`](https://github.com/JalapenoLabs/Anubis/tree/main/docs).

## License

MIT. Anubis is a Jalapeno Labs project.
