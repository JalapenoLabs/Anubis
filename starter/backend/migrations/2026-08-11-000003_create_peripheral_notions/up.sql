CREATE TABLE peripheral_notions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The ownership chain: every application record reaches a team.
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX peripheral_notions_team_id_idx ON peripheral_notions (team_id);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON peripheral_notions
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
