# Testing

The framework's tests are unit tests beside the code they cover, and integration suites in `anubis/tests/`. An application stamped by `anubis new` inherits the same shape: unit tests in its modules, one narrative per model in `backend/tests/`, mirrored by the starter.

## The suites

| Kind | Where | What it proves |
|---|---|---|
| Unit | `#[cfg(test)]` beside the code | One function's contract, no I/O |
| Narrative | `anubis/tests/<area>_flow.rs` | One area's story end to end, over HTTP, against real Postgres |
| Concurrency | `anubis/tests/concurrency_flow.rs` | What holds when two requests arrive at the same instant |
| Adversarial | `anubis/tests/adversarial_flow.rs` | What the surface does with inputs nobody sends by accident |
| Benchmark | `anubis/tests/login_storm.rs` | Latency under a burst; ignored by default |

A narrative is one linear story with a caller, a sequence, and assertions on what each step answered. It reads top to bottom and needs no shared fixtures beyond `tests/support/`.

Every suite that needs a database gates on `DATABASE_URL` and logs a skip when it is unset, so `cargo test` works on a machine without one. CI always provides it. See [ci.md](ci.md).

The starter's narratives are templates as well as tests: one is stamped for every scaffolded model, and CI runs the stamped copies on every push through the [scaffold proof](ci.md#the-scaffold-proof).

## Frontend tests

Both npm packages run Vitest over happy-dom, in the same shape: a `vitest.config.ts` naming the environment, the test glob, and a `vitest.setup.ts` that unmounts every render and restores every stubbed global after each test.

| Kind | Where | What it proves |
|---|---|---|
| Unit | `frontend/src/**/*.test.ts`, `starter/frontend/src/*.test.ts` | One pure function: url building, webhook signatures, WebAuthn encoding |
| Component | `frontend/src/fields/fields.test.tsx`, `starter/frontend/src/**/*.test.tsx` | What a person sees and clicks, rendered with Testing Library |

An application's component tests render through `frontend/src/testing/harness.tsx`, which is two functions:

- `renderWithProviders(ui, initialEntry)` mounts `ui` under the providers `main.tsx` mounts (router, HeroUI, the Anubis API client, i18n) at a chosen URL, with an SWR cache per render so one test's fetches never answer the next one's.
- `stubFetch(responses)` answers `fetch` from a table keyed by path, so the component under test meets the real ky client, the real status handling, and the real wire shapes. A path nobody listed answers 404, which turns a forgotten route into a failed assertion instead of a timeout.

Three tests establish the pattern, and all of them ship with `anubis new`: `App.test.tsx` proves the auth guard sends a signed-out visitor to `/sign-in` carrying the page they asked for, `pages/auth/SignInPage.test.tsx` proves the page renders one OAuth button per provider `GET /auth/oauth/providers` reports and offers sign-up only while `GET /auth/registration` says the deployment takes any, and `pages/auth/SignUpPage.test.tsx` proves the closed deployment gets the reason instead of a form.

## End-to-end tests

Component tests answer their own fetches. End-to-end tests answer nothing: Playwright drives Chromium against the running application, so a spec passes only when the browser, Vite, the Rust backend, and Postgres all agree. Everything lives in `starter/frontend`, and `anubis new` stamps it into every application.

| File | What it is |
|---|---|
| `playwright.config.ts` | Chromium project, base URL, artifacts on failure |
| `e2e/support/app.ts` | Sign-up, sign-in, the locale strings, and the form locator |
| `e2e/global-setup.ts` | Waits for the stack, and says how to start it when it never arrives |
| `e2e/*.spec.ts` | One narrative each |

Three specs are the proving set, and they are chosen the way the Rust narratives are, one story per area:

- `creative-concepts.spec.ts` is the golden path: register, create a creative concept, open it, add a tangible thing, see it listed. It is the scaffolder's own output, driven the way a person drives it.
- `auth.spec.ts` is the auth round trip: sign out, ask for a guarded URL, land on `/sign-in?next=…`, sign in, arrive at the page originally asked for. The guards and `sanitizeDestination` have unit coverage; only a browser proves the session cookie and the redirect chain agree.
- `settings.spec.ts` is a write that survives a reload, which is the cheapest way to tell a form that saved from one that only updated its own state.

### Running them

The suite drives a real stack, so a stack has to be running:

```sh
yarn dev                                          # Postgres, the backend, Vite
yarn workspace anubis-starter-frontend test:e2e   # in a second terminal
```

Or `yarn e2e` from the repository root, which starts all three, runs the specs, and stops the servers however the run ended. `yarn playwright install chromium` provisions the browser once per machine.

`yarn e2e` starts the backend with `RATE_LIMIT_DISABLED=true`, and CI does the same. Each spec registers an account, which is a rate no person reaches and the auth limiter refuses with a `429`; the limiter is exactly what [api.md](api.md#rate-limiting) says it is, a budget for suites that drive these endpoints hard from one address, and it has its own Rust narrative. Running the suite against a stack started by a plain `yarn dev` works until the third sign-up.

`E2E_BASE_URL` points the specs somewhere else, which is what CI sets and what running them against a deployed environment needs. Vite runs with `strictPort`, so 5173 being taken is an error rather than a quiet move to 5174: `APP_URL` names that origin, and every email link, OAuth redirect, and Stripe return is built from it.

The suite is Chromium only. Cross-browser coverage is a Playwright project matrix, and adding one is a decision about runner minutes rather than about test code: every spec is written against roles and labels, so they run unchanged on the day the matrix grows.

There is no Playwright `webServer` block. A run of this application is three processes and `webServer` starts one command, so a block that started Vite alone would report a healthy server while everything it proxies answered nothing. `global-setup.ts` polls `GET /healthz` through the base URL instead, which is one request that proves both halves, and fails with the command to run when the stack never comes up.

### The database

E2E is **not** isolated the way the Rust suites are. There is no `TestDatabase` here: the specs drive the development database, and the accounts and records they create stay there. What keeps runs out of each other's way is that every account is `e2e-<purpose>-<uuid>@example.com` and every record carries a random suffix, the same convention the older Rust narratives use. Nothing in the suite counts rows, so nothing it asserts depends on being alone.

Booting a database of its own per run is possible through the same `DATABASE_URL` the stack already reads, and is worth doing the day a spec needs to count something. It is not worth doing before then.

### Selectors

Specs address the interface the way a person does: by accessible role and visible text. The text comes from `src/locales/`, imported directly, so a renamed button fails at the locale key rather than silently matching nothing.

**No `data-testid`.** Not one is needed today, and the bar for adding one is a control genuinely unreachable by role, with the reason written at the call site. The one control that failed this bar, the avatar that opens the account menu, was given an `aria-label` instead: a name it should have had for screen readers anyway. That is the preferred fix every time, because it improves the application rather than annotating it for the tests.

Two Playwright details worth knowing when writing a spec here:

- An accessible name matches on **substring** unless `exact: true` is passed, which is why "Sign in" needs it and "Sign in with a passkey" is the reason.
- HeroUI renders a table as a `grid`, so its cells are `gridcell` and `rowheader`, not `cell`. Assert on the row (`getByRole('row').filter({ hasText })`), scoped to the table's own `aria-label` when a page has more than one.

A page can render several forms with identically labelled fields, a show page holds its own edit form and its children's create form, and forms have no accessible name. `formWithSubmit(page, submitLabel)` addresses one by its submit button, which is the control that says what the form is for.

### Flake

Retries are off and there are no sleeps. Playwright's auto-waiting is the only synchronization the specs use, so a spec that needs a sleep is reporting something real about the application. A retry would hide exactly that, and a suite whose failures are not believed is worse than no suite. Specs run one at a time (`workers: 1`) so that a failure reads as one story rather than three interleaved ones.

## Test-database isolation

`anubis/tests/support/TestDatabase` gives a test a Postgres database of its own. It opens a maintenance connection to the server `DATABASE_URL` names, creates a uniquely named database beside the one in that URL, runs the framework migrations into it, and hands out its URL. `Drop` drops the database, so a test that panics cleans up as surely as one that passes.

The reason is countability. A narrative that only reads rows it just wrote is safe on a shared database, and the older suites stay out of each other's way with random emails. A concurrency test is not: it asks how many admins a team has, how many deliveries a write produced, how many rows are left in the queue, and every one of those questions is only answerable when nothing else is writing.

Adoption is per suite and mechanical: replace the `DATABASE_URL` skip-gate and the `run_pending_migrations` call with `TestDatabase::create`, then pass `database.url()` wherever the suite passed `database_url`. The concurrency, adversarial, and benchmark suites use it. The older narratives do not, and there is no urgency: what they cost by sharing is nothing they currently assert.

Packages outside `anubis` cannot import that module, because an integration test's support module is private to its package. They rely instead on the migration lock below, which is what makes concurrent appliers against one shared database safe.

## Concurrent migrations

Applying migrations takes a Postgres advisory lock over one fixed key, held for the length of the pass. Two appliers issuing the same `CREATE TABLE` otherwise race in Postgres' catalog and the loser fails with a unique violation on `pg_type_typname_nsp_index`, which reads like nothing to do with migrations.

The lock covers two situations that are really one:

- **Production.** Two application instances booting at the same moment both call `run_pending_migrations`. The first applies, the second waits and then finds nothing pending.
- **Tests.** Rust runs the tests inside one binary on parallel threads, so two narratives calling the same setup helper apply migrations together against the same database.

`anubis::db`'s `MIGRATION_LOCK_KEY` documents the key and why it must never change.

## Writing a concurrency test

Three things make one worth having:

1. **Real parallelism.** `#[tokio::test(flavor = "multi_thread")]` and `tokio::spawn`, not a sequence with an `await` between the steps.
2. **A property, not a sequence.** Assert the invariant that has to survive ("the team keeps an admin", "no job is lost"), not the order the racers happened to finish in.
3. **A race that actually runs.** A scheduler usually runs one request to completion before starting the next, which means the race the test is named after never happens. Where the outcome depends on both racers being in flight, hold the lock they contend for from the test itself, let both queue behind it, and release: `simultaneous_demotions_keep_a_team_administrable` does exactly that.

## The login-storm benchmark

Verifying a password is argon2id, which is CPU-bound and memory-hard on purpose. Sign-in is therefore the one endpoint whose cost a stranger controls, and `anubis::auth::password` runs both hashing and verification through `spawn_blocking` so the work lands on the blocking pool rather than on a runtime worker.

`login_storm.rs` measures whether that separation holds: it fires concurrent sign-ins at a real socket and times `GET /healthz` throughout. It is `#[ignore]`d, because it is a measurement rather than an assertion, and it wants a release build and the rate limiter switched off:

```sh
DATABASE_URL=... RATE_LIMIT_DISABLED=true \
  cargo test --release -p anubis --test login_storm -- --ignored --nocapture
```

`--release` matters most: argon2 in a debug build is orders of magnitude slower than the one production runs. `RATE_LIMIT_DISABLED=true` matters because in production the limiter is the first line, refusing the eleventh credential attempt from one client per minute with a `429`; what the benchmark measures is the layer behind it, which is what a burst distributed across many addresses meets.

Reference numbers, 32 cores, release, one process:

| Measure | Value |
|---|---|
| One sign-in, uncontended | 18 ms |
| 64 concurrent sign-ins | p50 226 ms, p95 261 ms |
| Throughput | ~240 sign-ins/second |
| `/healthz`, quiet | p50 117 µs, p95 195 µs |
| `/healthz`, during the storm | p50 556 µs, p95 6.7 ms |

Read it this way: sign-in latency rising with the queue is the design working, because argon2 is meant to cost. `/healthz` staying in the sub-millisecond range is the isolation working. `/healthz` tracking the sign-in latency would mean the hashing had reached the runtime workers, and that would be a bug.

The storm is 64 sign-ins because that is the default `PASSWORD_HASH_CONCURRENCY`, the number of argon2 computations `password::Hasher` admits at once (see [rate limiting](api.md#rate-limiting)). At that size the gate is saturated and nothing is refused, so the table measures the queue. A larger storm measures the shedding path instead: the excess waits five seconds and is answered `503`.
