-- Requests received from Hypothetical Sender, stored before anything is done
-- with them.
--
-- Deliberately not team-owned: a provider posting an event is not signed in and
-- names no team. Which of the application's records an event belongs to is a
-- question only the processing job can answer, and it answers it from the
-- payload.
CREATE TABLE hypothetical_sender_webhooks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The provider's JSON, exactly as it arrived.
    payload JSONB NOT NULL,
    -- The request headers, minus the ones that would be credentials.
    headers JSONB NOT NULL,
    -- Whether the signature checked out. Recorded rather than enforced at the
    -- edge, so a request that fails verification is still evidence.
    verified BOOLEAN NOT NULL DEFAULT false,
    -- When the request arrived. This is the row's creation time, named for what
    -- it means, which is why the table carries no separate created_at.
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When processing finished. NULL while a row is still waiting for its job.
    processed_at TIMESTAMPTZ,
    -- The most recent processing failure, kept for whoever has to explain it.
    error TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The backlog: rows no job has finished with, oldest first. Partial, because
-- the processed rows are the ones nobody queries by time.
CREATE INDEX hypothetical_sender_webhooks_unprocessed_idx
    ON hypothetical_sender_webhooks (received_at)
    WHERE processed_at IS NULL;

CREATE TRIGGER set_updated_at BEFORE UPDATE ON hypothetical_sender_webhooks
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
