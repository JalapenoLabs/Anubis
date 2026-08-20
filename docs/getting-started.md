# Getting started

This is the newcomer path: install the CLI, stamp an application, run it, scaffold
a domain, and ship the image. Every command below was run while this page was
written, and the output shown is the output it printed.

By the end you will have a running SaaS application with teams, roles, sign-in,
a versioned public REST API, outgoing webhooks, and a CRUD feature you generated
with one command.

The rest of `docs/` is decision records: why each piece works the way it does.
This page is the order to meet them in. The [nine-minute demo](demo.md) is the
same ground at demo speed.

## Prerequisites

| Tool | Version | Why |
|---|---|---|
| Rust | 1.96.0, through [rustup](https://rustup.rs) | The toolchain `rust-toolchain.toml` pins; rustup installs it on first build |
| Node | 20 or newer (24.13.0 is what CI runs) | The frontend build and the dev server |
| Yarn | 4.17.0, through Corepack | Pinned by `packageManager`; `corepack enable` is the whole setup |
| Docker | any recent version | The development Postgres, and the production image |
| Git | any recent version | The framework installs from git until the packages publish |

```sh
rustup toolchain install 1.96.0
corepack enable
docker --version
```

Nothing else. The framework links no system libraries: Postgres is spoken in
Rust, TLS is rustls with its roots compiled in, so there is no `libpq` and no
OpenSSL to install.

## Install the CLI

Today, from git:

```sh
cargo install --git https://github.com/JalapenoLabs/Anubis.git anubis
```

That is a release build of the framework, so it takes a couple of minutes once
(96 seconds on the machine this page was written on).

<!-- Published-crate swap: replace the command above with `cargo install anubis`
     the day the crate is on crates.io. Nothing else on this page changes. -->
Once the crate is published this becomes `cargo install anubis`, and nothing
else on this page changes.

Check it:

```sh
anubis --help
```

```
A Rust-first SaaS framework: teams, roles, auth, a versioned REST API, and full-stack scaffolding.

Usage: anubis [COMMAND]

Commands:
  new       Stamp a new application from the starter template
  scaffold  Generate application code from the living templates
  eject     Copy a framework frontend component into the application to own it
  upgrade   Move this application to another framework release
  doctor    Verify toolchain, database, and config health
  routes    Print the framework route table
  roles     Validate and compile the application's roles.yml
  billing   Compile the application's billing.yml for the frontend
  openapi   Export the framework's OpenAPI 3.1 document as JSON
  client    Generate clients from the OpenAPI document
  secret    Mint the secrets an application's environment needs
```

## Create the application

```sh
anubis new acme-crm --license mit
cd acme-crm
```

```
created `acme-crm` (151 files)

Next steps:
  cd acme-crm
  git init
  yarn install
  yarn dev

`yarn dev` writes .env from .env.example on its first run.
Commit Cargo.lock and yarn.lock too: they are what pin your dependencies.
Put your name in the LICENSE copyright line.
```

The name takes lowercase letters, digits, and hyphens, and it names everything:
the crate, the npm workspace, the database, and the Docker image. `--license mit`
writes an MIT LICENSE and sets the manifest field; leave it off and the
application is private (`"license": "UNLICENSED"`, no LICENSE file).

What landed:

```
acme-crm/
├── backend/            the Rust server: src/, migrations/, tests/
├── frontend/           the React SPA: src/, e2e/
├── config/
│   ├── roles.yml       roles and per-model grants, compiled into both ends
│   └── billing.yml     the subscription plans this application sells
├── compose.yaml        the development Postgres
├── Dockerfile          the production image, one binary serving API and SPA
├── .env.example        the development environment, copied to .env on first run
├── .github/workflows/ci.yml
├── rust-toolchain.toml, clippy.toml, .yarnrc.yml, package.json, Cargo.toml
└── LICENSE, README.md
```

Templates ship no lockfiles. Your first install resolves `Cargo.lock` and
`yarn.lock`, and committing them is what pins the application.

## Run it

```sh
git init
yarn install
yarn dev
```

`yarn dev` does three things in order: copies `.env.example` to `.env` when
there is none, starts Postgres in Docker and waits for its healthcheck, then
runs the backend and the Vite dev server with that environment.

The application is at <http://localhost:5173>. The backend is on port 3000, and
Vite proxies every path the backend claims, so the SPA and the API stay
same-origin in development exactly as they are in production.

The first build compiles the whole dependency graph. Budget a few minutes for
it once (76 seconds on the machine this page was written on). Later builds
recompile your crate alone, about 8 seconds.

`.env` is the one place development configuration lives. The database container
and the backend both read it, so credentials are written once, and each stamped
application mints its own development password. It is git-ignored;
`.env.example` is the committed template, so a variable the whole team needs
goes there.

Migrations run at boot, the framework's and then the application's, under a
Postgres advisory lock. A freshly stamped application migrates itself on first
start, and a deploy carries its schema with it.

## Sign up

Open <http://localhost:5173>, choose **Sign up**, and register. That one action
creates the user, a personal organization, and a default team named General,
with you as admin of both. Solo use needs no tenancy ceremony at all.

Email goes to the backend log in development, so the verification link is in the
log line rather than an inbox. The same is true of password resets, invitations,
and sign-in codes.

Check the environment while you are here:

```sh
anubis doctor
```

```
[ ok ] rustc 1.96.0 (ac68faa20 2026-05-25)
[ ok ] cargo 1.96.0 (30a34c682 2026-05-25)
[ ok ] node v24.13.0
[ ok ] yarn 1.22.22
[ ok ] Docker version 26.1.1, build 4cf5afa
[warn] DATABASE_URL not set; `yarn dev` provides it for the dev database
[warn] ANUBIS_SECRET_KEY not set; secrets at rest use the public development key. Generate one with `anubis secret generate`
[ ok ] config/roles.yml is valid (4 roles)
[ ok ] config/billing.yml is valid (2 plans, free plan "free")

All good.
```

Both warnings are correct for development: `yarn dev` supplies `DATABASE_URL`
from `.env`, and secrets at rest fall back to a public development key.
Production refuses to boot without a real one. The yarn line reports whatever is
first on `PATH`; inside the application, `packageManager` and Corepack pin 4.17.0
regardless.

Mint the key now if you like:

```sh
anubis secret generate
```

```
WLbW+EDceC4gAZCAX3SLG0CfF+4w/cVM0pzVIIkta2s=
Set this as ANUBIS_SECRET_KEY. Rotating it makes every value sealed under the old key unreadable, so read docs/architecture.md before replacing a live one.
```

The key alone goes to stdout and the guidance to stderr, so
`ANUBIS_SECRET_KEY="$(anubis secret generate)"` captures exactly the key.

## Your first model

Model the domain first. Every application record chains its ownership back to a
Team, so the second argument is that chain: `Team` for a top-level model,
`<Parent>,Team` for one nested under another.

```sh
anubis scaffold model Company Team industry:text_field employees:number_field
```

```
scaffolded Company (owned by a team)

created:
  backend/src/companies/mod.rs
  backend/src/companies/model.rs
  backend/src/companies/routes.rs
  backend/tests/companies_flow.rs
  backend/migrations/2026-08-20-172033_create_companies/up.sql
  backend/migrations/2026-08-20-172033_create_companies/down.sql
  frontend/src/api/routes/companyRoutes.ts
  frontend/src/components/CompanyForm.tsx
  frontend/src/locales/models/companies.en-US.json
  frontend/src/pages/CompanyPage.tsx
  frontend/src/pages/CompaniesPage.tsx
updated:
  backend/src/schema.rs
  backend/src/lib.rs
  config/roles.yml
  frontend/src/roles.generated.ts
  frontend/src/i18n.ts
  frontend/src/urls.ts
  frontend/src/App.tsx
  frontend/src/components/AppShell.tsx

Every scaffolded model carries the living template's `name` (required text) and `description` (optional text) columns.
Wired end to end alongside them: industry (text_field), employees (number_field). Added columns are nullable, or non-null with a database default, so a later migration never strands existing rows.

Next steps:
  review the generated module, then boot the app to apply the migration
  cargo test
```

Eleven files created and eight updated, from one command, across both ends: a
migration, a Diesel model carrying the ownership chain, account CRUD handlers,
`/api/v1` handlers with their OpenAPI registrations, permission grants in
`roles.yml`, an integration test, a typed ky route module, a form, a list page,
a show page, the navigation entry, the routes, and a locale file.

Restart the backend so the migration applies. There is no file watcher: stop
`yarn dev` and start it again, and do the same after every scaffold that writes
a migration. Vite reloads the frontend on its own.

Then open **Companies** in the navigation. Create a record, open it, edit it,
delete it. The table, the search, and the pagination are already there.

Field types the generator accepts today are `text_field`, `text_area`,
`number_field`, `boolean`, `date_field`, and the two `super_select` association
spellings below. The [field component library](scaffolding.md#the-field-component-library)
ships eighteen controls; the generator's table is the subset the living
templates prove, and an unsupported type is refused by name with the list.

A nested model names its parent and attaches itself to that parent's show page:

```sh
anubis scaffold model Contact Company,Team title:text_field
```

```
scaffolded Contact (owned through Company)

created:
  backend/src/contacts/mod.rs
  ...
  frontend/src/components/ContactsSection.tsx
updated:
  ...
  frontend/src/pages/CompanyPage.tsx
```

A nested model owns a section (its table and its form) rather than pages of its
own, and the parent's show page renders it. Two levels is the limit today;
[a third](scaffolding.md#deferred-a-third-level-of-ownership) is designed and
deliberately not shipped.

## Add a field

The same generator adds one column to a model that already exists, and
propagates it everywhere the model reaches:

```sh
anubis scaffold field Contact last_contacted_on:date_field
```

```
added last_contacted_on (date_field) to Contact

created:
  backend/migrations/2026-08-20-172055_add_last_contacted_on_to_contacts/up.sql
  backend/migrations/2026-08-20-172055_add_last_contacted_on_to_contacts/down.sql
updated:
  backend/src/schema.rs
  backend/src/contacts/model.rs
  backend/src/contacts/routes.rs
  backend/tests/contacts_flow.rs
  frontend/src/api/routes/contactRoutes.ts
  frontend/src/components/ContactForm.tsx
  frontend/src/components/ContactsSection.tsx
  frontend/src/locales/models/contacts.en-US.json
not found, so not updated:
  frontend/src/pages/ContactsPage.tsx
  frontend/src/pages/ContactPage.tsx

The column is nullable, so the rows the table already holds stay valid.
```

Artifacts a model does not have are named rather than skipped quietly: a nested
model has a section and no pages. The migration, the schema, the record and
request structs, the handlers, the test, the wire type, the zod schema, the form
control, the table column, and the locale strings all move together.

What `scaffold field` still leaves to you is the model's `SORTABLE` and filter
whitelists. Which columns a list endpoint lets a caller order and search by is a
product decision with an index attached.

## Associations

Two spellings, exactly as Bullet Train writes them. The plural `_ids` is a
has-many-through and needs a join model first; the singular `_id` is a
belongs_to and declares a real foreign key.

```sh
anubis scaffold model Tag Team
anubis scaffold join AppliedTag 'company_id{class_name=Company}' 'tag_id{class_name=Tag}'
anubis scaffold field Company 'tag_ids:super_select{class_name=Tag}'
```

The join run ends by printing the field command that reads through it:

```
scaffolded AppliedTag (Company and Tag)

created:
  backend/src/applied_tags/mod.rs
  backend/src/applied_tags/model.rs
  backend/src/applied_tags/routes.rs
  backend/tests/applied_tags_flow.rs
  backend/migrations/2026-08-20-172047_create_applied_tags/up.sql
  backend/migrations/2026-08-20-172047_create_applied_tags/down.sql
  frontend/src/api/routes/appliedTagRoutes.ts
updated:
  backend/src/schema.rs
  backend/src/lib.rs

A join model is infrastructure, not a resource, so it takes no entry in config/roles.yml: reading the association is a read on Company, changing it is an update on Company, and listing a form's options is a read on Tag.

Next steps:
  boot the app to apply the migration
  anubis scaffold field Company tag_ids:super_select{class_name=Tag}
  cargo test
```

The field run writes no migration, because an association's values are rows in
the join table. What it does write is the multi-select on the form, the ids on
the wire type and both request bodies, the reconciliation in both write
handlers, the count on the table and the show page, and the label in the locale
file.

The belongs_to is the Bullet Train signature pattern, and it is advice rather
than a technicality: **assign to a team membership, not to a user**, so a
teammate who has been invited but has not signed up yet can already be assigned
work.

```sh
anubis scaffold field Company 'owner_id:super_select{class_name=TeamMembership}'
```

```
added owner_id (super_select) to Company

created:
  backend/migrations/2026-08-20-172054_add_owner_id_to_companies/up.sql
  backend/migrations/2026-08-20-172054_add_owner_id_to_companies/down.sql
updated:
  backend/src/schema.rs
  ...

The column is a nullable foreign key into team_memberships, indexed, and cleared rather than blocking when the record it points at is deleted. Options and writes both read the generated `valid_owners` method, so a form can only ever offer, and a request only ever store, a record of the caller's own team.
```

Invite a teammate from **Team settings**, then assign a company to them before
they claim the invitation. That is the model working: the membership exists from
the moment somebody is invited.

## OAuth sign-in in one line

```sh
anubis scaffold oauth google
```

```
Google sign-in needs no generated code: the sign-in page renders every provider this backend holds credentials for.

To enable it:
  register an OAuth client with Google, with this redirect URI:
    <APP_URL>/auth/oauth/google/callback
  set both credentials in the environment:
    GOOGLE_OAUTH_CLIENT_ID=...
    GOOGLE_OAUTH_CLIENT_SECRET=...
  restart the backend, and the button appears on the sign-in page

APP_URL must be the origin the browser sees, since it is the base of that redirect URI.
```

This command writes no file, deliberately. The whole flow (authorization code
with PKCE, server-side state and nonce, ID token verification, identity linking,
account bootstrap) is framework behavior, and the sign-in page renders one
button per provider `GET /auth/oauth/providers` reports. The environment decides
which providers exist, so a button that would always fail never appears.

In development the redirect URI is
`http://localhost:5173/auth/oauth/google/callback`, because `APP_URL` is the
origin the browser sees. Providers are OpenID Connect only; Google ships in the
registry.

## The public API

Every scaffolded model is on `/api/v1` from the moment it is generated. Mint a
token to use it: **Team settings**, then **Developers**, then create a platform
application. The token is shown exactly once.

```sh
curl -H "authorization: Bearer $TOKEN" http://localhost:3000/api/v1/companies
```

```json
{
  "companies": [
    {
      "id": "…", "team_id": "…",
      "name": "Northwind Traders", "description": null,
      "industry": "Logistics", "employees": 240,
      "tag_ids": [], "owner_id": null, "owner_label": null,
      "created_at": "…", "updated_at": "…"
    }
  ],
  "pagination": { "page": 1, "limit": 25, "total_items": 1, "total_pages": 1 }
}
```

Every column you scaffolded is there, associations included, because the API and
the browser read one serializer. The envelope is the same on every list endpoint:
the plural resource key plus a `pagination` object, with `?page=`, `?limit=`, and
`?sort=-created_at` as the conventions.

The browsable docs are at <http://localhost:3000/api/v1/docs> and the document
itself at `/api/v1/openapi.json`. A token belongs to its team and acts with that
team's admin role, so another team's record answers `404`, identically to one
that does not exist.

The application owns its OpenAPI document, so the generated TypeScript client
comes from the application's own binary:

```sh
cargo run -p acme-crm -- openapi > openapi.json
anubis client generate-ts --from openapi.json --out frontend/src/api/v1.generated.ts
```

```
wrote frontend/src/api/v1.generated.ts
```

The stamped CI workflow runs both and fails on a diff, so the client can never
lag the API. Run them after a scaffold. See [api.md](api.md).

## Webhooks

**Outgoing** need no setup. Every scaffolded model emits `company.created`,
`company.updated`, and `company.destroyed` from the functions its two surfaces
share, inside the transaction that wrote the row. A team subscribes an endpoint
in **Team settings**, then **Developers**, and the signing secret is shown once.
The delivery log there shows every attempt, its response code, and a redeliver
button.

**Incoming** are one command per provider:

```sh
anubis scaffold webhook stripe
```

```
scaffolded StripeWebhook (Stripe webhooks)

created:
  backend/src/stripe_webhooks/job.rs
  backend/src/stripe_webhooks/mod.rs
  backend/src/stripe_webhooks/model.rs
  backend/src/stripe_webhooks/routes.rs
  backend/tests/stripe_webhooks_flow.rs
  backend/migrations/2026-08-20-172101_create_stripe_webhooks/up.sql
  backend/migrations/2026-08-20-172101_create_stripe_webhooks/down.sql
updated:
  backend/src/schema.rs
  backend/src/lib.rs

A received webhook is stored first and processed afterwards, so nothing is lost and every attempt can be retried. The endpoint answers 200 as soon as the row is committed.

Next steps:
  boot the app to apply the migration
  register this endpoint with Stripe:
    <APP_URL>/webhooks/stripe
  set the shared secret in the environment:
    STRIPE_WEBHOOK_SECRET=...
  finish `verify_signature` in backend/src/stripe_webhooks/routes.rs: every provider signs differently, and the one shipped is the generic HMAC-SHA256 scheme
  finish `act_on` in backend/src/stripe_webhooks/job.rs, which is where a stored event becomes something your application did
  cargo test
```

Two functions are deliberate blanks, and the run names both: only you know how
your provider signs and what its events mean. Everything around them is
finished. See [webhooks.md](webhooks.md).

## Billing

`config/billing.yml` is what your application sells. The stamped file ships a
free plan and a Pro plan with placeholder Stripe price ids:

```yaml
plans:
  - key: free
    name: Free
    limits:
      seats: 3

  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_replace_me_pro_monthly
        amount: 2900
        currency: usd
        per_seat: true
    limits:
      seats: 25
```

Exactly one plan sells no prices, and that is the free plan: an organization
with no subscription row is on it, so plan resolution is always total. The
billing screen is at `/organizations/:organizationId/billing`, linked from
organization settings.

Until `STRIPE_SECRET_KEY` is set, every organization is on the free plan,
reading works, and the write endpoints answer `503` naming the variable. Turn it
on with a test-mode key and regenerate the frontend catalog after any edit:

```sh
anubis billing generate-ts --file config/billing.yml --out frontend/src/plans.generated.ts
```

`seats` is the one limit the framework enforces itself, at invitation creation.
Every other limit is yours to check at your own creation choke points. See
[billing.md](billing.md).

## Own a field component

`anubis eject` is the escape hatch, Bullet Train's `bin/resolve --eject`. It
copies a component out of the installed package into your application, stamps
where it came from, and rewires every import:

```sh
anubis eject --list
anubis eject TextField
```

```
ejected TextField from @jalapenolabs/anubis v0.1.0

created:
  frontend/src/anubis/fields/TextField.tsx
  frontend/src/anubis/fields/internal/TextualField.tsx
rewired:
  frontend/src/components/CompanyForm.tsx
  frontend/src/components/ContactForm.tsx
  ...

The extra files are package-internal: TextField reads them and the package does not export them, so they came along rather than being left behind an import that would not resolve.

TextField is yours now: upgrading @jalapenolabs/anubis no longer improves it, and a fix that lands upstream has to be brought over by hand. Delete the file to go back to the package's copy.
```

Deleting the copy is the way back. The ejectable surface is the field component
library alone: the API client, the realtime client, and the hooks stay
framework-owned, because a copy of those forks a wire protocol rather than a
style.

## Take a new release

Framework behavior lives in two versioned dependencies, the `anubis` crate and
the `@jalapenolabs/anubis` package, so upgrading is bumping two numbers and
re-running the generators. There is no starter template to merge and no
upstream branch to reconcile, which is the single biggest cost Bullet Train
applications pay. Install the CLI for the version you are moving to, then run
one command on a branch:

```sh
cargo install anubis --version 0.3.0
anubis upgrade --dry-run          # everything it would do, writing nothing
anubis upgrade                    # or: anubis upgrade --to 0.3.0
```

It rewrites both requirements, moves both lockfiles, re-runs all three
generators, and prints the release notes for the version it landed on. Read
them: pre-1.0 a minor release may break, and the notes are where a break and
the steps it takes by hand are named. Until the packages are published,
applications track the framework from git and the command says so; see
[upgrading.md](upgrading.md).

## Everyday commands

```sh
anubis doctor            # toolchain, database, and config health
anubis routes            # the framework's mounted route table
anubis roles check       # validate config/roles.yml standalone
anubis eject --list      # every component an application can own
anubis upgrade --dry-run # what moving to the latest release would do
anubis secret generate   # mint an ANUBIS_SECRET_KEY
```

`anubis routes` prints the framework's surface, 74 routes today, grouped by
area. Your own routes live in your router and are not visible to the CLI.

## Run the tests

```sh
cargo test --workspace            # the backend narratives, against Postgres
yarn test                         # Vitest over the frontend
yarn playwright install chromium  # once per machine
yarn e2e                          # Playwright, on a stack it starts and stops
```

Every scaffolded model arrives with a narrative test of its own: register an
account, create the record, read it, update it, delete it, and prove another
tenant cannot reach it. Suites that need a database gate on `DATABASE_URL` and
skip without one, so `cargo test` works on a machine with no Postgres running.

`yarn e2e` starts Postgres, the backend, and Vite, runs the specs, and stops the
servers however the run ended. See [testing.md](testing.md).

## Deploy

One binary serves the API and the built SPA. `Dockerfile` builds it in three
stages, and the runtime stage holds the binary, the bundle, and nothing else:

```sh
docker build -t acme-crm .
```

```sh
docker run --rm -p 3000:3000 \
  -e DATABASE_URL=postgres://user:password@db.internal/acme_crm \
  -e APP_URL=https://app.example.com \
  -e ANUBIS_SECRET_KEY="$(anubis secret generate)" \
  acme-crm
```

Both build stages install from lockfiles, so commit `Cargo.lock` and
`yarn.lock` before the first build.

The image bakes the deployment shape and none of its secrets:
`ANUBIS_ENV=production`, `HOST=0.0.0.0`, `PORT=3000`, and
`SPA_DIR=/app/public`. What the deployment supplies:

| Variable | Required | Meaning |
|---|---|---|
| `DATABASE_URL` | yes | Postgres, the system of record |
| `APP_URL` | yes | The public base URL; every email link, OAuth redirect, and Stripe return is built from it |
| `ANUBIS_SECRET_KEY` | yes in production | Base64 for 32 bytes, from `anubis secret generate` |
| `SMTP_URL`, `MAIL_FROM` | to send real email | Otherwise mail goes to the log |
| `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET` | to charge money | Otherwise every organization is on the free plan |
| `REDIS_URL` | for more than one instance | Fans realtime channels out across instances |
| `CORS_ALLOWED_ORIGINS` | for cross-origin API callers | Exact origins, comma separated |
| `TRUSTED_PROXY_HEADER` | behind a proxy | Names the header carrying the client address |

`SPA_DIR` is the whole switch for serving the frontend. Point it at the build
output and every path no router claims serves `index.html`, so a cold load of a
client-side route works. Leave it unset and the binary serves the API alone,
which is the shape for a bundle on a CDN.

Point the restart policy at `/healthz` and the load balancer's traffic gate at
`/readyz`. `SIGTERM` drains in-flight requests for 25 seconds, inside
Kubernetes' default grace period. See [server.md](server.md) and
[architecture.md](architecture.md#deployment).

Rotating `ANUBIS_SECRET_KEY` is a one-way door: every value sealed under the old
key stops opening, so second-factor enrollments are discarded and those users
enroll again. Treat a rotation as user communication, not just a deploy.

## Where to go next

- [The nine-minute demo](demo.md), the same ground at demo speed
- [Scaffolding](scaffolding.md), the generator family in full, and the anchor contract
- [Tenancy, teams, and organizations](tenancy.md), the model everything chains to
- [REST API](api.md), the contract, authentication, and rate limits
- [The server](server.md), what `anubis::server::serve` owns
- [Background jobs](jobs.md), [Webhooks](webhooks.md), [Realtime](realtime.md), [Billing](billing.md), [Email](email.md)
- [Upgrading](upgrading.md), the versioning policy and `anubis upgrade`
- [Testing](testing.md) and [CI](ci.md)

Bullet Train's method is worth stealing whole: write the scaffold commands in a
scratch file, review them with people before running them, run them, commit the
generated code in its own commit, then polish. Tearing down and re-scaffolding is
cheap. Living with a wrong domain model is not.
