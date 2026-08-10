CREATE TABLE invitations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Null for organization-level invitations.
    team_id UUID REFERENCES teams(id) ON DELETE CASCADE,
    -- The unclaimed membership created alongside a team invitation, so
    -- resources can be assigned to the invitee before they join.
    team_membership_id UUID REFERENCES team_memberships(id) ON DELETE CASCADE,
    roles TEXT[] NOT NULL DEFAULT '{}',
    invited_by UUID REFERENCES users(id) ON DELETE SET NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX invitations_organization_id_idx ON invitations (organization_id);
CREATE INDEX invitations_team_id_idx ON invitations (team_id);
CREATE INDEX invitations_email_idx ON invitations (email);
