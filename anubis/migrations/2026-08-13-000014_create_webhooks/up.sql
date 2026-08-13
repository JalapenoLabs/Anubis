-- Outgoing webhooks: a team subscribes an endpoint to event types, and every
-- matching domain event becomes one delivery per subscribed endpoint. Both
-- tables are written through the caller's own connection, so a subscription's
-- deliveries commit with the domain write that produced them.
CREATE TABLE webhook_endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    -- Where deliveries are POSTed. HTTPS outside development; see
    -- `anubis::webhooks` for the transport rules.
    url TEXT NOT NULL,
    -- What the team called this subscription, for their own debugging screen.
    description TEXT,
    -- The event types this endpoint wants, e.g. {project.created,project.updated}.
    -- An empty set receives nothing, which is what an endpoint mid-configuration
    -- should do.
    event_types TEXT[] NOT NULL DEFAULT '{}',
    -- A paused endpoint keeps its history and receives nothing new.
    active BOOLEAN NOT NULL DEFAULT TRUE,
    -- The signing secret, sealed with `anubis::auth::secret_box` under
    -- ANUBIS_SECRET_KEY. It is encrypted rather than hashed on purpose: the
    -- server has to recompute the HMAC of every request body it sends, which a
    -- one-way hash cannot do. The plaintext is shown to the team exactly once,
    -- at creation, and never leaves the server again.
    secret TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Every read of this table is "the endpoints of one team", and emission adds
-- "and active", which is the whole of the hot path.
CREATE INDEX webhook_endpoints_team_idx ON webhook_endpoints (team_id, active);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON webhook_endpoints
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- One attempted delivery of one event to one endpoint. The row is the whole
-- debugging story: what was sent, how often it was tried, what came back, and
-- why it stopped.
CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webhook_endpoint_id UUID NOT NULL
        REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    -- The serialized record, exactly as the REST API answers with it.
    payload JSONB NOT NULL,
    -- pending (queued or waiting on a retry), delivered, failed (the last
    -- attempt failed and another is coming), or dead (out of attempts).
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    -- The HTTP status the endpoint last answered with, when it answered at all.
    response_status INTEGER,
    last_error TEXT,
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The debugging screen reads one endpoint's deliveries, newest first.
CREATE INDEX webhook_deliveries_endpoint_idx
    ON webhook_deliveries (webhook_endpoint_id, created_at DESC);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
