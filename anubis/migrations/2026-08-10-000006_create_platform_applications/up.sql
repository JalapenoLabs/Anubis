CREATE TABLE platform_applications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX platform_applications_team_id_idx ON platform_applications (team_id);

CREATE TABLE platform_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    platform_application_id UUID NOT NULL
        REFERENCES platform_applications(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Null means the token does not expire; rotation is the revocation path.
    expires_at TIMESTAMPTZ
);

CREATE INDEX platform_tokens_application_idx ON platform_tokens (platform_application_id);
