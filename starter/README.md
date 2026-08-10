# Anubis starter application

The template `anubis new` stamps out, and the framework's CI host app.

## Development

Three processes: Postgres, the backend, and the frontend dev server.

```sh
# 1. Postgres (any Postgres 17 works; docker is the zero-setup path)
docker run -d --name app-pg -e POSTGRES_USER=app -e POSTGRES_PASSWORD=app \
  -e POSTGRES_DB=app_development -p 5432:5432 postgres:17.6

# 2. Backend (from the repository root). APP_URL points email links at the
#    vite dev server; in production the backend serves the SPA, so the
#    default is correct there.
DATABASE_URL=postgres://app:app@localhost:5432/app_development \
APP_URL=http://localhost:5173 \
cargo run -p anubis-starter

# 3. Frontend
cd starter/frontend && yarn dev
```

The backend applies pending migrations at boot. Emails (verification and
password reset) go to the backend log in development; the action links are in
the log lines.

The vite dev server proxies `/auth` and `/healthz` to the backend on port
3000, so the SPA and API stay same-origin in both development and production.
