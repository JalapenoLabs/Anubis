-- A user's account at an OpenID Connect provider. The provider's `sub` claim
-- is the stable identifier; an email address can change, a subject cannot.
CREATE TABLE oauth_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    subject TEXT NOT NULL,
    -- The address the provider asserted at link time, for support and audit.
    email TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One account per provider subject: a subject can never be claimed twice.
    UNIQUE (provider, subject),
    -- And one identity per provider per user, which also indexes user_id.
    UNIQUE (user_id, provider)
);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON oauth_identities
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- In-flight authorization-code flows, between the redirect out and the
-- callback back. The provider echoes an opaque state token; only its hash is
-- stored, so a leaked database yields no resumable flows.
CREATE TABLE oauth_states (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    -- Replayed into the ID token check, which is what binds the token to this
    -- flow.
    nonce TEXT NOT NULL,
    -- The PKCE verifier whose challenge went out with the redirect.
    pkce_verifier TEXT NOT NULL,
    -- Root-relative path the user was headed for, carried across the flow.
    destination TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
