-- Durable background work. A row is enqueued through the caller's own
-- connection, so it commits in the same transaction as the domain write that
-- caused it: no job can outlive a rolled-back write, and no committed write
-- loses its follow-up work.
CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Which worker pool takes this job. A job type declares its queue, so slow
    -- work never starves fast work.
    queue TEXT NOT NULL,
    -- The job type's stable identifier, which routes the payload to a handler.
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    -- Incremented when a worker claims the row, so a worker that dies mid-job
    -- still spends an attempt and a poisoned job cannot retry forever.
    attempts INTEGER NOT NULL DEFAULT 0,
    -- Copied from the job type at enqueue time: changing the constant later
    -- must not change the terms in-flight rows were accepted under.
    max_attempts INTEGER NOT NULL,
    -- Earliest time this job may run. Retries push it into the future.
    run_at TIMESTAMPTZ NOT NULL,
    -- Set while a worker holds the row. A claim older than the worker's lease
    -- is treated as abandoned and reclaimed.
    locked_at TIMESTAMPTZ,
    locked_by TEXT,
    -- The message from the most recent failure, for debugging live retries.
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The worker's claim query filters by queue and orders by run_at, so this is
-- the index its hot path rides.
CREATE INDEX jobs_claim ON jobs (queue, run_at);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON jobs
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Jobs that exhausted their attempts. They keep their original id, so a log
-- line about a running job still finds it here after it dies. Nothing deletes
-- from this table automatically: work that failed permanently is a fact an
-- operator should see, not garbage to collect.
CREATE TABLE dead_jobs (
    id UUID PRIMARY KEY,
    queue TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    attempts INTEGER NOT NULL,
    last_error TEXT,
    -- When the job was first enqueued, which with failed_at gives the full
    -- span it spent trying.
    enqueued_at TIMESTAMPTZ NOT NULL,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest failures first is how this table is always read.
CREATE INDEX dead_jobs_failed_at ON dead_jobs (failed_at DESC);
