CREATE TABLE creative_concepts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The ownership chain: every application record reaches a team.
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX creative_concepts_team_id_idx ON creative_concepts (team_id);

-- updated_at is maintained by the framework's shared trigger function; the
-- scaffolder attaches it to every generated table.
CREATE TRIGGER set_updated_at BEFORE UPDATE ON creative_concepts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
