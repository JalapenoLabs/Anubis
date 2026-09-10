-- The sub-tenant tier sits between an organization and its teams. It owns the
-- work, its own membership, and its own teams; the organization above it stays
-- the billing and policy umbrella. Every organization has exactly one to begin
-- with, so an application that never surfaces the tier still has a complete
-- ownership chain and nothing existing changes shape.

CREATE TABLE sub_tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A team scopes itself to a sub-tenant and to an organization at once, so
    -- the pair has to be referenceable. This is what makes a team scoped to
    -- another organization's sub-tenant impossible in the schema rather than
    -- only in the queries that read it.
    UNIQUE (id, organization_id)
);

CREATE INDEX sub_tenants_organization_id_idx ON sub_tenants (organization_id);

CREATE TABLE sub_tenant_memberships (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sub_tenant_id UUID NOT NULL REFERENCES sub_tenants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    roles TEXT[] NOT NULL DEFAULT '{}',
    -- A suspension is a deny, not a missing grant: it cuts the member out of
    -- this sub-tenant whatever else would have let them in.
    suspended_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (sub_tenant_id, user_id)
);

CREATE INDEX sub_tenant_memberships_user_id_idx ON sub_tenant_memberships (user_id);

-- NULL is an organization-level team, inherited by every sub-tenant; a set
-- value scopes the team to one sub-tenant and it never leaks to the others.
-- The composite reference is what forbids scoping a team to a sub-tenant that
-- belongs to a different organization.
ALTER TABLE teams
    ADD COLUMN sub_tenant_id UUID,
    ADD CONSTRAINT teams_sub_tenant_fkey
        FOREIGN KEY (sub_tenant_id, organization_id)
        REFERENCES sub_tenants (id, organization_id) ON DELETE CASCADE;

CREATE INDEX teams_sub_tenant_id_idx ON teams (sub_tenant_id);

-- Organization membership carries the two facts sub-tenant resolution reads:
-- whether the member reaches every sub-tenant, and whether they are cut.
-- 'full' is the default, so every existing member keeps the reach they had.
ALTER TABLE organization_memberships
    ADD COLUMN access TEXT NOT NULL DEFAULT 'full'
        CHECK (access IN ('full', 'guest')),
    ADD COLUMN suspended_at TIMESTAMPTZ;

CREATE TRIGGER set_updated_at BEFORE UPDATE ON sub_tenants
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER set_updated_at BEFORE UPDATE ON sub_tenant_memberships
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Every organization that already exists gets its default sub-tenant, so an
-- application stamped before this migration reads exactly like one stamped
-- after it. Existing teams stay organization-level, which is the reach they
-- have today: they are inherited by every sub-tenant, including this one.
INSERT INTO sub_tenants (organization_id, name)
SELECT id, 'Main' FROM organizations;
