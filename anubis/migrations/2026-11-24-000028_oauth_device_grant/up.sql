-- The device authorization grant (RFC 8628), for programs on machines without
-- a browser: the program shows a short code, and the person approves it from
-- any browser. See `docs/oauth-server.md`.
--
-- Both codes are stored as their SHA-256 only, like every other credential in
-- the authorization server.

-- Which grants a client declared, in RFC 7591's vocabulary. A client declaring
-- nothing gets the authorization code flow (RFC 7591 section 2), and every
-- grant may be refreshed, so `refresh_token` is always present.
ALTER TABLE oauth_clients
    ADD COLUMN grant_types TEXT[] NOT NULL
        DEFAULT ARRAY['authorization_code', 'refresh_token']
        CONSTRAINT oauth_clients_grant_types CHECK (
            grant_types <@ ARRAY[
                'authorization_code',
                'urn:ietf:params:oauth:grant-type:device_code',
                'refresh_token'
            ]
        );

-- A cached metadata document's grants are read only when it is fetched, so
-- every cached document is fetched again on its next use rather than keeping
-- the default above until its cache entry is due.
UPDATE oauth_clients SET refresh_after = now() WHERE kind = 'metadata_document';

-- One device authorization: the codes a program was handed, how it polls, and
-- what the person decided.
--
-- The person decides through the consent screen like any other request, so
-- each row has exactly one `oauth_authorization_requests` row beside it until
-- the decision consumes that row. The decision lands here: `grant_id` for an
-- approval, `denied_at` for a refusal. The program then exchanges the device
-- code once, which sets `used_at`; presenting it again revokes the grant.
CREATE TABLE oauth_device_codes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    client_id UUID NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    -- The program's polling credential, 256 random bits.
    device_code_hash TEXT NOT NULL UNIQUE,
    -- The short code the person types. Unique so that one entry names one
    -- authorization; a collision is drawn again.
    user_code_hash TEXT NOT NULL UNIQUE,
    -- How often the program may poll, grown by every poll that came too soon.
    interval_seconds INTEGER NOT NULL CHECK (interval_seconds > 0),
    last_polled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    grant_id UUID REFERENCES oauth_grants(id) ON DELETE CASCADE,
    denied_at TIMESTAMPTZ,
    used_at TIMESTAMPTZ,
    CONSTRAINT oauth_device_codes_one_decision CHECK (grant_id IS NULL OR denied_at IS NULL),
    CONSTRAINT oauth_device_codes_spent_when_approved CHECK (used_at IS NULL OR grant_id IS NOT NULL)
);

-- Expired rows are swept as new ones are made.
CREATE INDEX oauth_device_codes_expires_at ON oauth_device_codes (expires_at);
CREATE INDEX oauth_device_codes_client_id ON oauth_device_codes (client_id);
CREATE INDEX oauth_device_codes_grant_id ON oauth_device_codes (grant_id);

-- An authorization request is delivered one of two ways: back to a redirect
-- URI with a code (the authorization code flow), or to a device that polls
-- (this grant). A device request has no redirect URI, no `state`, and no PKCE
-- challenge, because the device code is what proves the program. It belongs
-- to the account that last entered its user code (`entered_by`), and no other
-- account may read or decide it, so passing the request's id to somebody is
-- no way around typing the code. Deleting that account leaves the request to
-- nobody until the code is entered again, rather than taking it away from
-- the device.
ALTER TABLE oauth_authorization_requests
    ALTER COLUMN redirect_uri DROP NOT NULL,
    ALTER COLUMN code_challenge DROP NOT NULL,
    ADD COLUMN device_code_id UUID UNIQUE REFERENCES oauth_device_codes(id) ON DELETE CASCADE,
    ADD COLUMN entered_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD CONSTRAINT oauth_authorization_requests_delivery CHECK (
        (
            device_code_id IS NULL
            AND redirect_uri IS NOT NULL
            AND code_challenge IS NOT NULL
            AND entered_by IS NULL
        )
        OR (
            device_code_id IS NOT NULL
            AND redirect_uri IS NULL
            AND code_challenge IS NULL
            AND state IS NULL
        )
    );
