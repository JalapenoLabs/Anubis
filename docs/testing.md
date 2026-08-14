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
