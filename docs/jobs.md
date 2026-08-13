# Background jobs

Durable background work lives in PostgreSQL, through `anubis::jobs`. Redis stays out of the durability path entirely: it is a cache and a fanout, never the record of what still has to happen.

The reason the queue is in Postgres is the transaction. `jobs::enqueue` writes through the caller's own connection, so a job enqueued inside a transaction commits with the domain write that caused it. A rolled-back write leaves no job behind, and a committed write never loses its follow-up work. A queue in another datastore can only approximate that with outbox tables and reconciliation.

## Defining a job

A job is a serializable struct plus the terms it runs under:

```rust
#[derive(Serialize, Deserialize)]
struct SendWelcomeEmail {
    user_id: Uuid,
}

impl anubis::jobs::Job for SendWelcomeEmail {
    const KIND: &'static str = "send_welcome_email";
    // Optional, shown with their defaults:
    const QUEUE: &'static str = "default";
    const MAX_ATTEMPTS: i32 = 5;
}
```

`KIND` is data, not a symbol: it is stored on every row and routes the payload back to a handler, so renaming it strands the rows already enqueued under the old name. `MAX_ATTEMPTS` is copied onto the row at enqueue time, so changing the constant never changes the terms in-flight jobs were accepted under.

Payloads carry ids, not records. A job may run minutes after it was enqueued, and by then the database is the only current view of a row.

## Enqueueing

```rust
jobs::enqueue(connection, &SendWelcomeEmail { user_id }).await?;
jobs::enqueue_in(connection, &SendWelcomeEmail { user_id }, Duration::from_hours(1)).await?;
```

Both take a connection rather than a pool, which is what lets them join a transaction already in progress. A delay is a floor, not a schedule: the job runs once a worker reaches it after that point.

## Running jobs

```rust
let worker = Worker::builder(pool)
    .register(move |job: SendWelcomeEmail| {
        let mailer = mailer.clone();
        async move { deliver_welcome(&mailer, job.user_id).await }
    })
    .build();

tokio::spawn(worker.run(async {
    let _ = tokio::signal::ctrl_c().await;
}));
```

Registration is the single source of truth. Annotating the closure's parameter infers the job type, the closure captures whatever the handler needs, and registering a job subscribes the worker to that job's queue. A queue can never be served by a worker that cannot run its jobs, and a queue name is never repeated at a second site to drift from the first.

The knobs and their defaults:

| Builder method | Default | Meaning |
|---|---|---|
| `concurrency` | 8 | Jobs run at once |
| `poll_interval` | 1s | How long the worker sleeps on an empty queue |
| `lease` | 5 minutes | How long a claim is honored before another worker may reclaim the job |

`run` returns once its `shutdown` future resolves and every in-flight job has finished. Draining is what makes a deploy safe: a job that has started records its outcome instead of being reclaimed and run a second time.

## Delivery guarantees

Delivery is at-least-once, so **handlers must be idempotent**. A worker claims a row by stamping it with a lease and spending one attempt, so a worker killed mid-job cannot retry forever, and the same property means work can run twice when a lease expires under a handler that is still going. Give the lease room for the slowest handler registered on it.

Claiming selects candidate rows `FOR UPDATE SKIP LOCKED` and stamps them in the same transaction, so any number of workers draw from one table without two of them ever taking the same row.

A handler fails its job by returning an error, by panicking, or by receiving a payload that no longer decodes. Failures retry on a quadratic backoff from a ten second base, capped at an hour: 10s, 40s, 90s, 160s, 250s. A dependency that blips recovers on the first retry; one that is genuinely down is not hammered while it comes back.

## When work stops retrying

A job that exhausts its attempts moves to the `dead_jobs` table with its last error, its original id, and the time it was first enqueued. Nothing deletes from that table automatically: work that failed permanently is a fact an operator should see, not garbage to collect.

A payload whose kind no attached handler recognizes is dead-lettered on its first claim rather than retried. Workers only draw from queues they registered jobs for, so an unrecognized kind is not a routing accident: no handler in the running build can execute it, and retrying would hide that.

Both tables are ordinary Postgres tables and are meant to be queried:

- `jobs` is work that still has to happen. `attempts`, `run_at`, and `last_error` say where a struggling job stands.
- `dead_jobs` is work that will not happen without a person. `failed_at` and `enqueued_at` bracket how long it tried.

## The framework's own jobs

| Job | Queue | What it does |
|---|---|---|
| `anubis::webhooks::DeliverWebhook` | `webhooks` | POSTs one signed outgoing webhook delivery, recording the answer on its row |

Registering it is three lines on the worker builder, and the starter's `main.rs` shows them. It runs on its own queue so a customer endpoint that hangs for ten seconds cannot hold up an application's own work. [Outgoing webhooks](webhooks.md) covers the rest.

## Roadmap

Tracked in GitHub issues under M5:

- SMTP mail send through this queue.
- Recurring schedules, for maintenance work such as pruning expired sessions, tokens, and delivered webhooks.
- An in-app view of pending and dead jobs, sharing the webhook debugging UI.
