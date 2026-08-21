CREATE TABLE granular_details (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The ownership chain continues through the parent, never around it: the
    -- team is read off the chain's root at query time rather than copied here.
    tangible_thing_id UUID NOT NULL REFERENCES tangible_things(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX granular_details_tangible_thing_id_idx
    ON granular_details (tangible_thing_id);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON granular_details
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
