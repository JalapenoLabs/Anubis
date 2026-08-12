# anubis-starter

An application built on [Anubis](https://github.com/JalapenoLabs/Anubis), the
Rust-first SaaS framework: teams, roles, auth, a versioned REST API, and
full-stack scaffolding.

## Development

```sh
yarn install
yarn dev
```

`yarn dev` starts Postgres in Docker (waiting for health), the backend with
the right environment, and the frontend dev server. The backend applies
pending migrations at boot; the app is at http://localhost:5173.

Emails (verification, password reset, invitations, sign-in codes) go to the
backend log in development; the action links and codes are in the log lines.

## Layout

- `backend/`: the Rust application server (Axum on tokio, via the anubis crate)
- `frontend/`: the React SPA (Vite, HeroUI, Tailwind)
- `config/roles.yml`: the application's roles and permissions, compiled into
  both backend authorization and frontend affordances
- `compose.yaml`: the development Postgres

## Everyday commands

```sh
anubis doctor          # verify toolchain, database, and config health
anubis routes          # print the framework route table
anubis roles check     # validate config/roles.yml
```

Framework documentation lives in the
[Anubis repository](https://github.com/JalapenoLabs/Anubis/tree/main/docs).
