CREATE TABLE incidental_linkages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Both sides cascade: deleting either record takes its links with it.
    creative_concept_id UUID NOT NULL REFERENCES creative_concepts(id) ON DELETE CASCADE,
    peripheral_notion_id UUID NOT NULL REFERENCES peripheral_notions(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One row per pair, so attaching the same record twice is a no-op rather
    -- than a duplicate. Its index also serves lookups by creative concept,
    -- which is the leading column.
    UNIQUE (creative_concept_id, peripheral_notion_id)
);

CREATE INDEX incidental_linkages_peripheral_notion_id_idx
    ON incidental_linkages (peripheral_notion_id);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON incidental_linkages
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
