-- The audit log: who did what, written on the connection that did it.
--
-- One row per recorded act. Rows are inserted through the caller's own
-- connection, so an event commits with the write that caused it and a
-- rolled-back write leaves nothing behind, exactly as outgoing webhook
-- deliveries do.
--
-- The table is append-only by design: nothing in the framework updates or
-- deletes a row, and no endpoint offers either. There is deliberately no
-- `updated_at` and no `set_updated_at` trigger, because a row that could be
-- touched after the fact is not evidence.
CREATE TABLE audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The tenant the act happened in. Null for an account event, which belongs
    -- to the person rather than to any team: changing a password happens
    -- outside every team the account is a member of.
    team_id UUID REFERENCES teams(id) ON DELETE CASCADE,
    organization_id UUID REFERENCES organizations(id) ON DELETE CASCADE,
    -- Who acted. Null for an act the system performed on nobody's behalf, and
    -- null again once a deleted account takes its row with it, which is what
    -- the denormalized name below survives.
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    -- The actor as they read at the moment they acted, copied rather than
    -- joined. A log that renders "(deleted user)" where a name belongs has
    -- lost the answer it exists to give. Null for a system act.
    actor_name TEXT,
    -- What happened. A scaffolded model records the bare lifecycle action
    -- (`created`, `updated`, `destroyed`) and names itself in subject_type; a
    -- framework surface records a dotted verb (`member.role_changed`).
    action TEXT NOT NULL,
    -- What it happened to: the model name, then its id and its label. The id
    -- is null when the subject is gone or never had one; the label is what a
    -- reader recognizes, copied for the same reason actor_name is.
    subject_type TEXT NOT NULL,
    subject_id UUID,
    subject_label TEXT,
    -- The fields that moved, as {"field": {"old": ..., "new": ...}}. Secrets
    -- are absent because they are never serialized into it, not because they
    -- are stripped afterwards: the change set is computed from the same view
    -- the REST API answers with, which carries no secret to begin with.
    changes JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- The `x-request-id` of the request that did this, so a row in this table
    -- and the line in the request log are the same story.
    request_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The team-scoped listing, which is the only way the log is read: one team's
-- events, newest first. The subject_type and actor filters narrow inside that
-- window rather than opening one of their own.
CREATE INDEX audit_events_team_idx ON audit_events (team_id, created_at DESC);

-- The account listing, and the actor filter's half of the team listing.
CREATE INDEX audit_events_actor_idx ON audit_events (user_id, created_at DESC);
