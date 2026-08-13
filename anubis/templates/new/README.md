# anubis-starter

An application built on [Anubis](https://github.com/JalapenoLabs/Anubis), the
Rust-first SaaS framework: teams, roles, auth, a versioned REST API, and
full-stack scaffolding.

## Development

```sh
yarn install
yarn dev
```

`yarn dev` copies `.env.example` to `.env` when there is none, starts Postgres
in Docker (waiting for health), then runs the backend and the frontend dev
server with that environment. The backend applies pending migrations at boot;
the app is at http://localhost:5173.

`.env` is the one place development configuration lives: the database
container and the backend both read it. `.env.example` is the committed
template, so add a variable there when the whole team needs it, and keep
machine-specific values in `.env`, which is git-ignored.

Emails (verification, password reset, invitations, sign-in codes) go to the
backend log in development; the action links and codes are in the log lines.

## Layout

- `backend/`: the Rust application server (Axum on tokio, via the anubis crate)
- `frontend/`: the React SPA (Vite, HeroUI, Tailwind)
- `config/roles.yml`: the application's roles and permissions, compiled into
  both backend authorization and frontend affordances
- `compose.yaml`: the development Postgres
- `.env.example`: the development environment, copied to `.env` on first run
- `.github/workflows/ci.yml`: format, lint, test, build, and the two drift
  checks that keep the generated files honest

## Everyday commands

```sh
anubis doctor            # verify toolchain, database, and config health
anubis routes            # print the framework route table
anubis roles check       # validate config/roles.yml
anubis secret generate   # mint an ANUBIS_SECRET_KEY
```

## First commit

Commit `Cargo.lock` and `yarn.lock` along with the rest. Templates ship
without them, so they are resolved once on your first install and pinned from
then on; CI's `yarn install --immutable` needs `yarn.lock` to be there.

## License

The stamped `package.json` says `UNLICENSED`, which keeps a private
application private. `anubis new <name> --license mit` writes an MIT LICENSE
instead, with the current year and a placeholder for your name.

Framework documentation lives in the
[Anubis repository](https://github.com/JalapenoLabs/Anubis/tree/main/docs).
