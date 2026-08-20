# The nine-minute demo

Bullet Train's flagship demo is nine hours: build and ship a SaaS product live.
This is the nine-minute cut. It builds a small CRM (Companies, Contacts, Tags,
work assigned to a teammate) and hits every moment that makes people lean
forward: one command produces full-stack CRUD, one command adds a field and it
turns up in eleven files, an association becomes a multi-select, the public API
was already there, webhooks fire on a write, and the billing screen renders from
a configuration file.

The script below is rehearsed. Every command was run and every duration was
measured; the [measurements](#measured-durations) are at the end, so you can see
what the clock is made of.

## What the clock includes

Nine minutes is talking time. The generator's own share of it is **2.9 seconds**:
the ten commands in this script, timed end to end. What fills the rest is
narration, the browser, and the backend restart after each migration, which is
about ten seconds.

Warm the machine before you record, exactly as any screencast does:

```sh
cargo install --git https://github.com/JalapenoLabs/Anubis.git anubis-framework   # ~96s, once
anubis new warmup && cd warmup && yarn install && cargo build           # ~76s, once
```

Cold, add roughly three minutes for the first dependency build and the first
install. Doing that on camera proves nothing except that Rust compiles.

## Before you record

- Docker running, and no other Postgres on port 54321.
- A browser window at 1280x800 with one tab, zoomed so text reads on video.
- A terminal beside it, large font, prompt shortened to `$`.
- A warm cargo cache and a completed `yarn install` (above).
- An empty database. `docker compose -f compose.yaml down -v` between takes.
- A second browser profile if you want to show an invitation being claimed.

## The timeline

| Time | Beat | The moment |
|---|---|---|
| 0:00 | Stamp and boot | 151 files, one command, and it runs |
| 0:45 | Sign up | A personal organization and a team, with no ceremony |
| 1:30 | **The first model** | One command, both ends, live in the browser |
| 2:30 | **Add a field** | One column lands in eleven files |
| 3:15 | A nested model | It attaches itself to its parent's page |
| 4:15 | **The association** | A join, then a multi-select that scopes itself |
| 5:15 | Assign to a teammate | Assign work to somebody who has not signed up |
| 6:00 | **The public API** | It was there the whole time, docs and client included |
| 7:00 | Webhooks | A write fires an event, and the log shows the attempt |
| 7:45 | Billing | A pricing grid compiled from a YAML file |
| 8:30 | Close | The compile-time promise, and the image |

The three beats in bold are the ones to protect if you run long.

---

## 0:00 Stamp and boot

> "Every SaaS product rebuilds the same plumbing. Teams, roles, sign-in, a
> public API, webhooks, billing. Anubis is that plumbing, in Rust and React,
> with a generator that writes the rest."

```sh
anubis new acme-crm --license mit
cd acme-crm
```

```
created `acme-crm` (151 files)
```

**SHOT**: the file tree beside the one-line output.

```sh
yarn dev
```

> "Copies `.env.example` to `.env`, starts Postgres in Docker, applies the
> migrations, runs the backend and Vite."

Open <http://localhost:5173> while it starts.

## 0:45 Sign up

Register with any address. Email goes to the backend log in development, so the
verification link is right there in the terminal.

> "That one action created the user, a personal organization, and a team called
> General, with me as admin of both. Solo use costs no tenancy ceremony, and the
> multi-tenant model is already underneath."

**SHOT**: the signed-in shell, with the team switcher open.

## 1:30 The first model

> "Now the part that makes this worth the switch."

```sh
anubis scaffold model Company Team industry:text_field employees:number_field
```

Eleven files created and eight updated, all listed in the output, in less than
half a second.

**SHOT**: the full `created:` / `updated:` block. Let it sit for three seconds.
It is the single most persuasive frame in the demo.

> "A migration. A Diesel model with the ownership chain. Account CRUD handlers.
> The `/api/v1` handlers with their OpenAPI registrations. Permission grants in
> `roles.yml`, compiled into both ends. An integration test. On the frontend: a
> typed API module, a form, a list page, a show page, a locale file, the
> navigation entry, and the routes."

Restart the backend so the migration applies, then click **Companies**.

Create a record. Search it. Page through it. Open it. Edit it.

**SHOT**: the list page, then the show page.

> "Nobody wrote that. And because both ends are statically typed, a scaffold
> either compiles end to end or tells you exactly what to fix. That is the
> promise Rails cannot make."

## 2:30 Add a field

```sh
anubis scaffold field Company renewal_on:date_field
```

**SHOT**: the output, then the form, then the table, then the show page.

> "One column. It landed in the migration, the schema, three Rust structs, both
> request bodies, both write handlers, the integration test, the wire type, the
> zod schema, the form control, the table column, the show page, and the locale
> file. This is what Bullet Train calls the compounding feature: a field added
> in month six costs what a field added on day one costs."

Open `backend/tests/companies_flow.rs` and show the new assertions.

> "The test grew with it. A column that reaches the database but not the test is
> a column nothing proves."

## 3:15 A nested model

```sh
anubis scaffold model Contact Company,Team title:text_field
```

Point at the `updated:` line naming `frontend/src/pages/CompanyPage.tsx`.

> "The second argument is the ownership chain, and every record chains back to a
> team. A nested model owns a section rather than pages, and it wired itself
> into the page the last scaffold wrote."

Restart, open a company, add a contact inside it.

**SHOT**: the contacts table inside the company's show page.

## 4:15 The association

> "Tags on companies. That is a has-many-through, and it takes two commands,
> exactly as it does in Bullet Train."

```sh
anubis scaffold model Tag Team
anubis scaffold join AppliedTag 'company_id{class_name=Company}' 'tag_id{class_name=Tag}'
anubis scaffold field Company 'tag_ids:super_select{class_name=Tag}'
```

The join run ends by printing the field command that reads through it. Show
that.

Restart, create two tags, then edit a company and tag it.

**SHOT**: the multi-select with chips, then the tag count on the list page.

> "The options endpoint and the write validation read one generated method, so a
> form can only ever offer, and a request only ever store, a record of this
> team. Another tenant's id answers 400, not 500."

## 5:15 Assign to a teammate

Invite somebody from **Team settings**. Do not claim the invitation.

```sh
anubis scaffold field Company 'owner_id:super_select{class_name=TeamMembership}'
```

Restart, edit a company, and pick the invited person as its owner.

**SHOT**: the owner picker showing a person who has not signed up yet.

> "This is Bullet Train's signature advice and it is the framework's:
> assign to a membership, never to a user. A membership exists from the moment
> somebody is invited, so you can hand them work before they arrive, and it goes
> with them when they leave."

## 6:00 The public API

> "Here is the part nobody had to ask for."

**Team settings**, then **Developers**, then create a platform application. The
token appears once.

Open <http://localhost:3000/api/v1/docs>.

**SHOT**: the Scalar docs listing `companies`, `contacts`, and `tags`.

```sh
curl -H "authorization: Bearer $TOKEN" http://localhost:3000/api/v1/companies
```

> "Same serializer the browser reads, same permissions, same tenant scoping. The
> token belongs to the team, so another team's record answers 404, identically
> to one that does not exist."

Then the client:

```sh
cargo run -p acme-crm -- openapi > openapi.json
anubis client generate-ts --from openapi.json --out frontend/src/api/v1.generated.ts
```

> "The application owns its OpenAPI document, so the typed TypeScript client
> comes out of the application's own binary. CI regenerates it and fails on a
> diff, so the client can never lag the API."

## 7:00 Webhooks

**Developers**, then subscribe an endpoint to `company.created`. The signing
secret is shown once, in a modal you have to dismiss.

**SHOT**: the secret modal.

Create a company. Open the delivery log.

**SHOT**: the delivery row with its event type, status, attempt count, and
redeliver button.

> "Nothing was opted into. Every scaffolded model emits created, updated, and
> destroyed, from inside the transaction that wrote the row, so a rollback sends
> nothing and a commit never loses its event. The payload is the same shape the
> API answers with, signed HMAC-SHA256 over the timestamp and the body."

Optionally, the other direction, in one command:

```sh
anubis scaffold webhook stripe
```

> "A table, an endpoint that stores the request and queues a job in one
> transaction, the signature check, and the job. Two functions are deliberately
> blank and the run names both, because only you know how your provider signs
> and what its events mean."

## 7:45 Billing

Open `config/billing.yml`, then the billing screen at
`/organizations/:organizationId/billing`.

**SHOT**: the plans grid beside the YAML that produced it.

> "Plans are configuration, not rows. Exactly one plan sells no prices, and that
> is the free plan, which is what an organization with no subscription row is
> on, so plan resolution is always total. Stripe Checkout takes the purchase and
> the Stripe portal takes every change after it. This framework keeps only the
> projection it has to authorize from."

Point at the seat count.

> "Seats is the one limit the framework meters itself, at invitation creation,
> inside the invitation's transaction. Everything else is your product's to
> count."

## 8:30 Close

```sh
anubis routes
cargo test --workspace
```

> "Seventy-four framework routes, and every model you generated arrived with a
> narrative test that registers an account, writes the record, reads it, updates
> it, deletes it, and proves another tenant cannot reach it."

```sh
docker build -t acme-crm .
```

> "One binary serving the API and the bundle. No system libraries, because
> Postgres is spoken in Rust and TLS carries its own roots. Nine minutes ago
> this was an empty directory."

Land on the repository and [getting-started.md](getting-started.md).

---

## Measured durations

Every number below was measured while writing this page, on one developer
machine, with a warm cargo cache. Yours will differ; the shape will not.

| Step | Duration |
|---|---|
| `anubis new acme-crm --license mit` (151 files) | 0.14 s |
| `scaffold model Company Team industry employees` | 0.40 s |
| `scaffold field Company renewal_on:date_field` | 0.23 s |
| `scaffold model Contact Company,Team title` | 0.38 s |
| `scaffold model Tag Team` | 0.38 s |
| `scaffold join AppliedTag Company Tag` | 0.33 s |
| `scaffold field Company tag_ids` | 0.21 s |
| `scaffold field Company owner_id` | 0.30 s |
| `scaffold oauth google` | 0.12 s |
| `scaffold webhook stripe` | 0.47 s |
| **All ten, end to end** | **2.94 s** |
| Backend rebuild after a scaffold (app crate only) | 8.2 s |
| Backend boot, migrations applied, `/healthz` answering | 1.1 s |
| First dependency build, cold target directory | 76 s |
| `cargo install --git ... anubis-framework`, cold | 96 s |

And the request beats, against a debug build, so a release build is faster
everywhere and much faster on sign-up:

| Request | Duration |
|---|---|
| `POST /auth/register` (argon2id, personal org, default team) | 347 ms |
| `POST` a record on the account surface | 223 ms |
| `GET` a list page | 63 ms |
| `POST` a platform application, token minted | 78 ms |
| `GET /api/v1/...` with the bearer token | 129 ms |
| `GET /api/v1/openapi.json` | 60 ms |
| `POST` subscribe a webhook endpoint | 74 ms |
| `POST` a record that emits a webhook | 141 ms |
| `GET` the delivery log | 63 ms |
| `GET` the billing data | 69 ms |

## What not to promise

The demo is honest or it is worthless. Say what ships:

- **Field types the generator accepts** are `text_field`, `text_area`,
  `number_field`, `boolean`, `date_field`, and the two `super_select` spellings.
  The component library ships eighteen controls; the generator's table is the
  subset the living templates prove, and it grows by adding a row.
- **Two levels of ownership.** `Task Goal,Project,Team` is refused by name.
  [The design for a third](scaffolding.md#deferred-a-third-level-of-ownership)
  is written down.
- **No admin panel, no user impersonation, no onboarding wizard, no in-app
  notifications, no audit log, no drag-and-drop ordering.** Bullet Train has
  them; Anubis does not, yet.
- **One look.** Tailwind and HeroUI, no theme engine and no dark-mode toggle.
- **The navigation is a desktop navbar.** A mobile menu is follow-up work.
- **English only** ships, though every scaffolded string is already extracted
  into a per-model locale file.
- **The packages are not published yet.** Applications depend on the framework
  from git until they are.

If somebody asks for one of those, the answer is the roadmap, not a maybe.
