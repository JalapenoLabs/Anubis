# Anubis starter application

The template `anubis new` stamps out, and the framework's CI host app.

## Development

One command from the repository root:

```sh
yarn dev
```

It copies `.env.example` to `.env` when there is none, starts Postgres in
Docker (waiting for health), then runs the backend and the frontend dev server
with that environment. The backend applies pending migrations at boot; the app
is at http://localhost:5173.

`.env` is the one place development configuration lives: the database
container and the backend both read it, so the credentials are written once.
It is git-ignored; `.env.example` is the committed template, and it is the
same file `anubis new` stamps into every application.

Emails (verification, password reset, invitations, sign-in codes) go to the
backend log in development; the action links and codes are in the log lines.

The vite dev server proxies every path the backend claims (`/api`, `/auth`,
`/tenancy`, `/billing`, `/account`, `/developers`, `/webhooks`, `/users`,
`/realtime`, and the two probes) to port 3000, so the SPA and the API stay
same-origin in development, exactly as they are in production. A framework test
compares that list against the backend's reserved prefixes, so a new prefix
cannot work in production and 404 here.

## Production

One binary serves the API and the frontend, and `Dockerfile` builds the image
that ships it. In this repository that file is the template `anubis new` stamps,
not a buildable image: the starter here compiles against the framework crate in
the parent workspace, which sits outside the build context. Stamp an
application and it builds there, from the application root:

```sh
docker build -t acme-crm .

docker run --rm -p 3000:3000 \
  --add-host=host.docker.internal:host-gateway \
  -e DATABASE_URL=postgres://acme_crm:password@host.docker.internal:54321/acme_crm_development \
  -e APP_URL=http://localhost:3000 \
  -e ANUBIS_SECRET_KEY="$(anubis secret generate)" \
  acme-crm
```

The image bakes `SPA_DIR`, binds `0.0.0.0:3000`, runs as a non-root user, and
answers a container healthcheck on `/healthz`. What it does not bake is
configuration: `DATABASE_URL`, `APP_URL`, and `ANUBIS_SECRET_KEY` are the
deployment's, and production refuses to boot without them. Both build stages
install from lockfiles, so commit `Cargo.lock` and `yarn.lock`.

The `--add-host` line is what lets a container reach a database on the host, the
compose Postgres above among them. A container on the same compose network
reaches it by service name instead, with no extra flag.

Without Docker, build the SPA and point `SPA_DIR` at the build output:

```sh
yarn workspace anubis-starter-frontend build

DATABASE_URL=postgres://user:password@host/app \
APP_URL=https://app.example.com \
ANUBIS_SECRET_KEY="$(anubis secret generate)" \
ANUBIS_ENV=production \
SPA_DIR=starter/frontend/dist \
cargo run --release -p anubis-starter
```

Every path the routers do not claim serves `index.html`, so client-side routes
survive a cold load; hashed files under `assets/` are served with a one-year
immutable cache and `index.html` with `no-cache`, so a deploy is live on the
next page load. An unmatched path under `/api`, `/auth`, `/tenancy`,
`/developers`, `/users`, or `/account` answers a JSON `404` instead of the
page.

Responses are compressed on the way out, brotli or gzip by negotiation, the
bundle and the API's JSON alike.

Leaving `SPA_DIR` unset serves the API alone; production warns at startup when
it does.

## OAuth sign-in

Add a provider with one command, from this directory:

```sh
anubis scaffold oauth google
```

It writes the button on the sign-in page and prints what it cannot do for you:
register an OAuth client with the provider, using
`http://localhost:5173/auth/oauth/google/callback` as the redirect URI in
development, then set `GOOGLE_OAUTH_CLIENT_ID` and `GOOGLE_OAUTH_CLIENT_SECRET`
in the backend's environment. `APP_URL` is the base of that redirect URI, so it
has to be the origin the browser sees, which is why the commands above set it
to the dev server rather than the backend. Until both credentials are set the
button lands back on the sign-in page with `oauth_unavailable`.

## End-to-end tests

```sh
yarn playwright install chromium   # once per machine
yarn e2e                           # from the repository root
```

`yarn e2e` starts Postgres, the backend, and Vite, runs the Playwright specs in
`frontend/e2e/`, and stops the servers however the run ended. With a stack
already running, `yarn workspace anubis-starter-frontend test:e2e` drives it
directly. See [testing.md](../docs/testing.md#end-to-end-tests).

## Running pieces individually

```sh
# Postgres only
docker compose --env-file .env -f starter/compose.yaml up -d --wait

# Backend, with the environment from .env (from the repository root)
yarn dotenv -- cargo run -p anubis-starter

# Frontend
yarn workspace anubis-starter-frontend dev
```
