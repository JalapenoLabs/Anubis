CREATE TABLE user_mfa (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    totp_secret TEXT NOT NULL,
    -- Null until the user proves they can generate codes; only confirmed
    -- rows gate login.
    confirmed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_recovery_codes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX user_recovery_codes_user_id_idx ON user_recovery_codes (user_id);
