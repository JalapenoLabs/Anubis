-- Stripe billing events: what Stripe said, stored before anything is made of it.
--
-- The framework receives Stripe's subscription events at its own endpoint,
-- writes them down, and processes them on the job queue afterwards. This is the
-- same store-then-process discipline `anubis scaffold webhook` generates for an
-- application's own receivers; see `docs/webhooks.md` and `docs/billing.md`.
CREATE TABLE stripe_billing_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Stripe's own id for the event, e.g. `evt_1Q...`. Unique, because Stripe
    -- redelivers an event it did not hear a 2xx for, and a redelivery must
    -- become one stored row and one job rather than a second of each.
    stripe_event_id TEXT NOT NULL,
    -- The event's type, e.g. `customer.subscription.updated`. Stored beside the
    -- payload so the backlog can be read without opening every document.
    event_type TEXT NOT NULL,
    -- Stripe's JSON, exactly as it arrived.
    payload JSONB NOT NULL,
    -- When Stripe created the event, which is the ordering key the subscription
    -- upsert compares against. Nullable only because it is read out of the
    -- payload, and a document without it is still worth keeping.
    stripe_created_at TIMESTAMPTZ,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When processing finished, or NULL while the row is still waiting.
    processed_at TIMESTAMPTZ,
    -- The most recent processing failure, kept on the row as evidence.
    error TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotency, enforced by the database rather than by the endpoint: two
-- concurrent redeliveries of one event cannot both win this insert.
CREATE UNIQUE INDEX stripe_billing_events_event_idx
    ON stripe_billing_events (stripe_event_id);

-- Every operational read is "what has not been processed yet".
CREATE INDEX stripe_billing_events_backlog_idx
    ON stripe_billing_events (received_at)
    WHERE processed_at IS NULL;

CREATE TRIGGER set_updated_at BEFORE UPDATE ON stripe_billing_events
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- When the event that last wrote this row was created at Stripe.
--
-- Stripe does not promise events arrive in the order they happened, so this is
-- what keeps a late `customer.subscription.updated` from undoing a cancellation
-- that reached us first: the upsert writes only when the incoming event is not
-- older than the one already applied. NULL means the row predates the event
-- loop, which any event or reconciliation is then free to overwrite.
ALTER TABLE subscriptions ADD COLUMN stripe_event_at TIMESTAMPTZ;
