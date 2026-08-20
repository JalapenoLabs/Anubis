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

## Production

One binary serves the API and the frontend, and `Dockerfile` builds the image
that ships it, from this directory:

```sh
docker build -t anubis-starter .

docker run --rm -p 3000:3000 \
  --add-host=host.docker.internal:host-gateway \
  -e DATABASE_URL=postgres://user:password@host.docker.internal:54321/anubis_starter_development \
  -e APP_URL=http://localhost:3000 \
  -e ANUBIS_SECRET_KEY="$(anubis secret generate)" \
  anubis-starter
```

Three stages build it and none of them ships: the Rust toolchain, the Node
toolchain, and a runtime holding the binary, the frontend bundle, and nothing
else. Migrations and `config/*.yml` are compiled into the binary, TLS roots
come with it, and Postgres is spoken in Rust, so the image installs no
libraries at all.

The image bakes `SPA_DIR`, binds `0.0.0.0:3000`, runs as a non-root user, and
answers a container healthcheck on `/healthz`. What it does not bake is
configuration: `DATABASE_URL`, `APP_URL`, and `ANUBIS_SECRET_KEY` belong to the
deployment, and production refuses to boot without them. `--add-host` is what
lets the container reach a database on the host, the compose Postgres among
them; a container on the same compose network reaches it by service name
instead.

Both build stages install from lockfiles, so `Cargo.lock` and `yarn.lock` have
to be committed. The image tags are pinned; keep the `FROM rust:` line and
`rust-toolchain.toml` in step when you bump either.

## Tests

```sh
yarn test                              # Rust-free: Vitest over the frontend
cargo test --workspace                 # the backend narratives, against Postgres
yarn playwright install chromium       # once per machine
yarn e2e                               # the browser suite, on a stack it starts
```

`yarn e2e` starts Postgres, the backend, and Vite, runs the Playwright specs in
`frontend/e2e/`, and stops the servers however the run ended. With a stack
already running, `yarn workspace anubis-starter-frontend test:e2e` drives it
directly.

## Layout

- `backend/`: the Rust application server (Axum on tokio, via the anubis crate)
- `frontend/`: the React SPA (Vite, HeroUI, Tailwind)
- `config/roles.yml`: the application's roles and permissions, compiled into
  both backend authorization and frontend affordances
- `compose.yaml`: the development Postgres
- `Dockerfile`: the production image, one binary serving the API and the SPA
- `.env.example`: the development environment, copied to `.env` on first run
- `.github/workflows/ci.yml`: format, lint, test, build, and the three drift
  checks that keep the generated files honest

## Upgrading Anubis

Framework behavior lives in two versioned dependencies, the `anubis` crate in
`backend/Cargo.toml` and `@jalapenolabs/anubis` in `frontend/package.json`, so
upgrading is bumping two numbers and re-running the generators. There is no
template to merge back in. Install the CLI for the version you are moving to,
then run one command on a branch:

```sh
cargo install anubis --version 0.3.0
anubis upgrade --dry-run   # everything it would do, writing nothing
anubis upgrade             # or: anubis upgrade --to 0.3.0
```

It rewrites both requirements, moves `Cargo.lock` and `yarn.lock`, regenerates
the three files CI checks for drift, and prints the release notes for the
version it landed on. Read them: Anubis is pre-1.0, a minor release may break,
and the notes are where a break and the steps it takes by hand are named. What
an upgrade never touches is the files this repository was stamped with, the CI
workflow and the Dockerfile among them: those are yours.

## Everyday commands

```sh
anubis doctor            # verify toolchain, database, and config health
anubis routes            # print the framework route table
anubis roles check       # validate config/roles.yml
anubis upgrade --dry-run # what moving to the latest release would do
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
