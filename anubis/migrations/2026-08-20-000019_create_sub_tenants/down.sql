ALTER TABLE organization_memberships
    DROP COLUMN suspended_at,
    DROP COLUMN access;

ALTER TABLE teams
    DROP CONSTRAINT teams_sub_tenant_fkey,
    DROP COLUMN sub_tenant_id;

DROP TABLE sub_tenant_memberships;
DROP TABLE sub_tenants;
