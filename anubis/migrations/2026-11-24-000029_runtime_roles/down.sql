-- Going back narrows and never widens. A role maps back to its roles.yml key
-- only while it still means exactly that key: one the organization made, or
-- one whose permissions it edited, has no key granting the same and no more,
-- so its holders lose it rather than gain something it never granted. The
-- default role is everybody's, so every membership takes `default` back on
-- the same condition.

ALTER TABLE organization_memberships ADD COLUMN roles TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE sub_tenant_memberships ADD COLUMN roles TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE team_memberships ADD COLUMN roles TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE invitations ADD COLUMN roles TEXT[] NOT NULL DEFAULT '{}';

-- The keys an organization's default role maps back to: `default` while it
-- still follows roles.yml, nothing once the organization edited it.
CREATE TEMPORARY TABLE unedited_default_keys ON COMMIT DROP AS
SELECT organization_id, ARRAY['default'] AS keys
FROM roles
WHERE key = 'default' AND permissions IS NULL;

UPDATE organization_memberships
SET roles = COALESCE(
    (
        SELECT array_agg(roles.key ORDER BY roles.key)
        FROM role_grants
        JOIN roles ON roles.id = role_grants.role_id
        WHERE role_grants.organization_membership_id = organization_memberships.id
            AND roles.key IS NOT NULL
            AND roles.permissions IS NULL
    ),
    '{}'
) || COALESCE(
    (
        SELECT keys FROM unedited_default_keys
        WHERE unedited_default_keys.organization_id = organization_memberships.organization_id
    ),
    '{}'
);

UPDATE sub_tenant_memberships
SET roles = COALESCE(
    (
        SELECT array_agg(roles.key ORDER BY roles.key)
        FROM role_grants
        JOIN roles ON roles.id = role_grants.role_id
        WHERE role_grants.sub_tenant_membership_id = sub_tenant_memberships.id
            AND roles.key IS NOT NULL
            AND roles.permissions IS NULL
    ),
    '{}'
) || COALESCE(
    (
        SELECT keys FROM unedited_default_keys
        JOIN sub_tenants ON sub_tenants.organization_id = unedited_default_keys.organization_id
        WHERE sub_tenants.id = sub_tenant_memberships.sub_tenant_id
    ),
    '{}'
);

-- Team-wide grants only: a grant narrowed to one record had no spelling in a
-- membership's roles, so it is dropped rather than widened to the team.
UPDATE team_memberships
SET roles = COALESCE(
    (
        SELECT array_agg(roles.key ORDER BY roles.key)
        FROM role_grants
        JOIN roles ON roles.id = role_grants.role_id
        WHERE role_grants.team_membership_id = team_memberships.id
            AND role_grants.resource_id IS NULL
            AND roles.key IS NOT NULL
            AND roles.permissions IS NULL
    ),
    '{}'
) || COALESCE(
    (
        SELECT keys FROM unedited_default_keys
        JOIN teams ON teams.organization_id = unedited_default_keys.organization_id
        WHERE teams.id = team_memberships.team_id
    ),
    '{}'
);

UPDATE invitations
SET roles = team_memberships.roles
FROM team_memberships
WHERE team_memberships.id = invitations.team_membership_id;

UPDATE invitations
SET roles = COALESCE(
    (
        SELECT array_agg(roles.key ORDER BY roles.key)
        FROM invitation_roles
        JOIN roles ON roles.id = invitation_roles.role_id
        WHERE invitation_roles.invitation_id = invitations.id
            AND roles.key IS NOT NULL
            AND roles.permissions IS NULL
    ),
    '{}'
) || COALESCE(
    (
        SELECT keys FROM unedited_default_keys
        WHERE unedited_default_keys.organization_id = invitations.organization_id
    ),
    '{}'
)
WHERE invitations.team_id IS NULL;

DROP TABLE invitation_roles;
DROP TABLE role_grants;
DROP TABLE roles;

ALTER TABLE organizations DROP COLUMN offered_role_keys;

ALTER TABLE invitations DROP CONSTRAINT invitations_id_organization_unique;
ALTER TABLE team_memberships DROP CONSTRAINT team_memberships_id_team_unique;
ALTER TABLE teams DROP CONSTRAINT teams_id_organization_unique;
ALTER TABLE sub_tenant_memberships DROP CONSTRAINT sub_tenant_memberships_id_sub_tenant_unique;
ALTER TABLE organization_memberships DROP CONSTRAINT organization_memberships_id_organization_unique;
