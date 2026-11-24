-- Runtime roles: an organization's roles become rows it can name, colour,
-- order, and edit, and a grant of one becomes a row hung off the membership
-- that gives the holder their reach. The `roles TEXT[]` columns this replaces
-- are converted below, key for key, and then dropped.

-- The composite keys every grant's foreign keys point at. Each makes a grant
-- in one organization naming a role, a membership, or a tenant of another
-- impossible in the schema rather than only in the queries that write one.
ALTER TABLE organization_memberships
    ADD CONSTRAINT organization_memberships_id_organization_unique UNIQUE (id, organization_id);
ALTER TABLE sub_tenant_memberships
    ADD CONSTRAINT sub_tenant_memberships_id_sub_tenant_unique UNIQUE (id, sub_tenant_id);
ALTER TABLE teams
    ADD CONSTRAINT teams_id_organization_unique UNIQUE (id, organization_id);
ALTER TABLE team_memberships
    ADD CONSTRAINT team_memberships_id_team_unique UNIQUE (id, team_id);
ALTER TABLE invitations
    ADD CONSTRAINT invitations_id_organization_unique UNIQUE (id, organization_id);

CREATE TABLE roles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    -- The roles.yml role this one was seeded from, or null for a role the
    -- organization made. `default` and `admin` are the two every organization
    -- holds: the role everybody has, and the administrator role at the top.
    key TEXT,
    -- Null follows roles.yml (its `name`, or the key made readable).
    name TEXT,
    -- Null follows roles.yml (its `color`, or the framework's neutral one).
    color TEXT,
    -- Higher outranks lower. The default role is always 0 and the
    -- administrator role always the highest; positions stay dense.
    position INTEGER NOT NULL CHECK (position >= 0),
    -- Permission keys (`<Model>.<action>`). Null follows roles.yml's
    -- definition of `key`, so a model scaffolded later reaches every
    -- organization's unedited roles; editing materializes the set.
    permissions TEXT[],
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT roles_id_organization_unique UNIQUE (id, organization_id),
    CONSTRAINT roles_organization_key_unique UNIQUE (organization_id, key),
    -- Deferrable, because a reorder moves every position in one statement
    -- and passes through states where two roles briefly share one.
    CONSTRAINT roles_organization_position_unique UNIQUE (organization_id, position)
        DEFERRABLE INITIALLY IMMEDIATE,
    -- A role the organization made has nothing to follow, so it states all
    -- three itself.
    CONSTRAINT roles_custom_is_complete
        CHECK (key IS NOT NULL OR (name IS NOT NULL AND color IS NOT NULL AND permissions IS NOT NULL)),
    CONSTRAINT roles_color_is_hex CHECK (color IS NULL OR color ~ '^#[0-9a-f]{6}$')
);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON roles
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- One grant of one role to the holder of one membership. The membership is
-- the scope: an organization membership grants organization-wide, a
-- sub-tenant membership in that sub-tenant, a team membership in that team,
-- and a team membership with a resource in that one record of the team.
-- Hanging the grant off the membership is what retires it with the
-- membership: somebody removed and invited back starts from what the new
-- invitation says, never from a grant the old membership held.
CREATE TABLE role_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL,
    role_id UUID NOT NULL,
    organization_membership_id UUID,
    sub_tenant_id UUID,
    sub_tenant_membership_id UUID,
    team_id UUID,
    team_membership_id UUID,
    -- An application record inside the team. The framework cannot see the
    -- application's tables, so this is never a foreign key; a grant naming a
    -- record of another team is inert, because a grant is only read through
    -- the membership of the team that owns the record in hand.
    resource_id UUID,
    granted_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The scope as one pair, for reading. Derived, so it can never disagree
    -- with the membership it is read from.
    scope_type TEXT GENERATED ALWAYS AS (
        CASE
            WHEN resource_id IS NOT NULL THEN 'resource'
            WHEN team_membership_id IS NOT NULL THEN 'team'
            WHEN sub_tenant_membership_id IS NOT NULL THEN 'sub_tenant'
            ELSE 'organization'
        END
    ) STORED,
    scope_id UUID GENERATED ALWAYS AS (
        COALESCE(resource_id, team_id, sub_tenant_id, organization_id)
    ) STORED,
    FOREIGN KEY (role_id, organization_id)
        REFERENCES roles (id, organization_id) ON DELETE CASCADE,
    FOREIGN KEY (organization_membership_id, organization_id)
        REFERENCES organization_memberships (id, organization_id) ON DELETE CASCADE,
    FOREIGN KEY (sub_tenant_id, organization_id)
        REFERENCES sub_tenants (id, organization_id) ON DELETE CASCADE,
    FOREIGN KEY (sub_tenant_membership_id, sub_tenant_id)
        REFERENCES sub_tenant_memberships (id, sub_tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (team_id, organization_id)
        REFERENCES teams (id, organization_id) ON DELETE CASCADE,
    FOREIGN KEY (team_membership_id, team_id)
        REFERENCES team_memberships (id, team_id) ON DELETE CASCADE,
    CONSTRAINT role_grants_one_holder CHECK (
        num_nonnulls(organization_membership_id, sub_tenant_membership_id, team_membership_id) = 1
    ),
    CONSTRAINT role_grants_sub_tenant_pair CHECK (
        (sub_tenant_membership_id IS NULL) = (sub_tenant_id IS NULL)
    ),
    CONSTRAINT role_grants_team_pair CHECK ((team_membership_id IS NULL) = (team_id IS NULL)),
    -- A record is reached through the team that owns it. Records an
    -- organization owns directly will be held through the organization
    -- membership instead, which is the one place this check widens.
    CONSTRAINT role_grants_resource_in_team CHECK (
        resource_id IS NULL OR team_membership_id IS NOT NULL
    ),
    CONSTRAINT role_grants_unique UNIQUE NULLS NOT DISTINCT (
        role_id,
        organization_membership_id,
        sub_tenant_membership_id,
        team_membership_id,
        resource_id
    )
);

CREATE INDEX role_grants_organization_membership_idx
    ON role_grants (organization_membership_id) WHERE organization_membership_id IS NOT NULL;
CREATE INDEX role_grants_sub_tenant_membership_idx
    ON role_grants (sub_tenant_membership_id) WHERE sub_tenant_membership_id IS NOT NULL;
CREATE INDEX role_grants_team_membership_idx
    ON role_grants (team_membership_id) WHERE team_membership_id IS NOT NULL;
CREATE INDEX role_grants_organization_idx ON role_grants (organization_id);
-- A team's roster and its administrators are read by team alone.
CREATE INDEX role_grants_team_idx ON role_grants (team_id) WHERE team_id IS NOT NULL;

-- The roles an organization invitation offers. A team invitation needs none:
-- it pre-creates the membership it will hand over, and that membership holds
-- its grants already. A role deleted while offered drops out of the offer.
CREATE TABLE invitation_roles (
    invitation_id UUID NOT NULL,
    role_id UUID NOT NULL,
    organization_id UUID NOT NULL,
    PRIMARY KEY (invitation_id, role_id),
    FOREIGN KEY (invitation_id, organization_id)
        REFERENCES invitations (id, organization_id) ON DELETE CASCADE,
    FOREIGN KEY (role_id, organization_id)
        REFERENCES roles (id, organization_id) ON DELETE CASCADE
);

CREATE INDEX invitation_roles_organization_idx ON invitation_roles (organization_id);

-- The roles.yml keys this organization has been offered. A recommended role
-- arrives once: deleting it never brings it back on the next request.
ALTER TABLE organizations ADD COLUMN offered_role_keys TEXT[] NOT NULL DEFAULT '{}';

-- Every organization gets a role for every key its people hold, plus the two
-- every organization has. The rows carry no name, colour, or permissions, so
-- each follows roles.yml exactly as the key it replaces did. The default role
-- is at the bottom, the administrator role at the top, and the rest between
-- them by key; an administrator reorders them once.
INSERT INTO roles (organization_id, key, position)
SELECT
    organization_id,
    key,
    row_number() OVER (
        PARTITION BY organization_id
        ORDER BY key <> 'default', key = 'admin', key
    ) - 1
FROM (
    SELECT organization_id, unnest(roles) AS key FROM organization_memberships
    UNION
    SELECT sub_tenants.organization_id, unnest(sub_tenant_memberships.roles)
    FROM sub_tenant_memberships
    JOIN sub_tenants ON sub_tenants.id = sub_tenant_memberships.sub_tenant_id
    UNION
    SELECT teams.organization_id, unnest(team_memberships.roles)
    FROM team_memberships
    JOIN teams ON teams.id = team_memberships.team_id
    UNION
    SELECT organization_id, unnest(roles) FROM invitations
    UNION
    SELECT id, 'default' FROM organizations
    UNION
    SELECT id, 'admin' FROM organizations
) AS held;

UPDATE organizations
SET offered_role_keys = (
    SELECT array_agg(roles.key ORDER BY roles.key)
    FROM roles
    WHERE roles.organization_id = organizations.id
);

-- Each key a membership held becomes a grant at that membership's scope. The
-- default role is held by everybody who reaches a scope, so it is never
-- granted: a member who held only `default` holds exactly that still.
INSERT INTO role_grants (organization_id, role_id, organization_membership_id)
SELECT DISTINCT organization_memberships.organization_id, roles.id, organization_memberships.id
FROM organization_memberships
CROSS JOIN LATERAL unnest(organization_memberships.roles) AS held (key)
JOIN roles
    ON roles.organization_id = organization_memberships.organization_id
    AND roles.key = held.key
WHERE held.key <> 'default';

INSERT INTO role_grants (organization_id, role_id, sub_tenant_id, sub_tenant_membership_id)
SELECT DISTINCT sub_tenants.organization_id, roles.id, sub_tenants.id, sub_tenant_memberships.id
FROM sub_tenant_memberships
JOIN sub_tenants ON sub_tenants.id = sub_tenant_memberships.sub_tenant_id
CROSS JOIN LATERAL unnest(sub_tenant_memberships.roles) AS held (key)
JOIN roles ON roles.organization_id = sub_tenants.organization_id AND roles.key = held.key
WHERE held.key <> 'default';

-- Unclaimed team memberships included: an invitation in flight keeps the
-- roles it offers on the membership the claim hands over.
INSERT INTO role_grants (organization_id, role_id, team_id, team_membership_id)
SELECT DISTINCT teams.organization_id, roles.id, teams.id, team_memberships.id
FROM team_memberships
JOIN teams ON teams.id = team_memberships.team_id
CROSS JOIN LATERAL unnest(team_memberships.roles) AS held (key)
JOIN roles ON roles.organization_id = teams.organization_id AND roles.key = held.key
WHERE held.key <> 'default';

INSERT INTO invitation_roles (invitation_id, role_id, organization_id)
SELECT DISTINCT invitations.id, roles.id, invitations.organization_id
FROM invitations
CROSS JOIN LATERAL unnest(invitations.roles) AS held (key)
JOIN roles ON roles.organization_id = invitations.organization_id AND roles.key = held.key
WHERE invitations.team_id IS NULL
    AND held.key <> 'default';

ALTER TABLE organization_memberships DROP COLUMN roles;
ALTER TABLE sub_tenant_memberships DROP COLUMN roles;
ALTER TABLE team_memberships DROP COLUMN roles;
ALTER TABLE invitations DROP COLUMN roles;
