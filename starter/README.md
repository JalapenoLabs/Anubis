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

The vite dev server proxies `/auth`, `/tenancy`, and `/healthz` to the backend
on port 3000, so the SPA and API stay same-origin in both development and
production.

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
