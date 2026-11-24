-- Resource grants: an organization shares the records it owns directly with
-- its teams. A team is granted every record of one kind in the organization,
-- or specific ones, and its members reach exactly what it was granted. What a
-- member may do there still comes from their roles; a grant is reach, never a
-- role.
--
-- The records are the application's, in tables the framework cannot see, so
-- `resource_id` is never a foreign key. A grant naming a record that no longer
-- exists is inert: access is only ever resolved with the record in hand, and
-- a record id is never issued twice.
CREATE TABLE resource_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL,
    team_id UUID NOT NULL,
    -- The model the records are, spelled as roles.yml spells it, so the
    -- permissions a member holds on them are `<kind>.<action>`. Framework
    -- subjects are namespaced (`Tenancy::Role`) and can never be one.
    kind TEXT NOT NULL CHECK (kind ~ '^[A-Za-z][A-Za-z0-9_]*$'),
    -- One record of the kind, or null for every record of it the organization
    -- owns, now and later.
    resource_id UUID,
    granted_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The team must be the organization's, which makes a grant across
    -- organizations impossible in the schema rather than only in the queries
    -- that write one, and deleting the team (or the organization) deletes its
    -- grants with it.
    FOREIGN KEY (team_id, organization_id)
        REFERENCES teams (id, organization_id) ON DELETE CASCADE,
    CONSTRAINT resource_grants_unique UNIQUE NULLS NOT DISTINCT (team_id, kind, resource_id)
);

-- Resolving access reads one organization's grants of one kind, for the
-- record in hand or for every record of the kind.
CREATE INDEX resource_grants_organization_kind_idx
    ON resource_grants (organization_id, kind, resource_id);
