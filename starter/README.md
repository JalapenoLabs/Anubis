# Anubis starter application

The template `anubis new` stamps out, and the framework's CI host app.

## Development

One command from the repository root:

```sh
yarn dev
```

It starts Postgres in Docker (waiting for health), the backend with the right
environment, and the frontend dev server. The backend applies pending
migrations at boot; the app is at http://localhost:5173.

Emails (verification, password reset, invitations, sign-in codes) go to the
backend log in development; the action links and codes are in the log lines.

The vite dev server proxies `/auth`, `/tenancy`, `/account`, `/users`, and
`/healthz` to the backend on port 3000, so the SPA and API stay same-origin in
development. Same-origin
production serving (the backend shipping the built SPA) is on the roadmap;
until then the built frontend needs its own static host.

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

## Running pieces individually

```sh
# Postgres only
docker compose -f starter/compose.yaml up -d --wait

# Backend (from the repository root)
DATABASE_URL=postgres://app:app@localhost:54321/app_development \
APP_URL=http://localhost:5173 \
cargo run -p anubis-starter

# Frontend
yarn workspace anubis-starter-frontend dev
```
