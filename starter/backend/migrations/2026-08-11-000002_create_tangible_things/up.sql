CREATE TABLE tangible_things (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The ownership chain continues through the parent, never around it.
    creative_concept_id UUID NOT NULL REFERENCES creative_concepts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX tangible_things_creative_concept_id_idx
    ON tangible_things (creative_concept_id);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON tangible_things
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
