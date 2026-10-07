-- This deployment as an OAuth 2.1 authorization server, for third-party
-- clients such as Claude Code and Codex connecting to its MCP endpoint as a
-- signed-in person. See `docs/oauth-server.md`.
--
-- Every secret here (authorization codes, access tokens, refresh tokens) is
-- stored as its SHA-256 only, the same discipline sessions follow, so a leaked
-- database yields nothing a client could present.

-- A client that has asked to act for somebody.
--
-- Two kinds. A `metadata_document` client is identified by the https URL of a
-- Client ID Metadata Document, which this server fetched; the row caches what
-- the document said until `refresh_after`. A `dynamic` client registered itself
-- through Dynamic Client Registration (RFC 7591) and was handed a random id.
-- The kind is recorded rather than inferred from the id's shape, which is what
-- the CIMD draft asks of a server supporting both.
CREATE TABLE oauth_clients (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    client_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('metadata_document', 'dynamic')),
    -- Self-asserted by the client. The consent screen shows it beside the
    -- redirect host, which is the part a client cannot lie about.
    name TEXT NOT NULL,
    client_uri TEXT,
    redirect_uris TEXT[] NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When a metadata document is fetched again. Null for a dynamic client,
    -- whose registration is the whole record.
    refresh_after TIMESTAMPTZ
);

-- Dynamic registrations nobody completed a grant with are swept by age.
CREATE INDEX oauth_clients_dynamic_created_at ON oauth_clients (created_at)
    WHERE kind = 'dynamic';

-- An authorization request waiting on the person's consent.
--
-- `GET /oauth/authorize` validates the request and stores it here, then sends
-- the browser to the consent screen with this row's id. Nothing the client
-- sent rides through the browser a second time, so nothing about it can be
-- edited on the way.
CREATE TABLE oauth_authorization_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    client_id UUID NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL,
    scopes TEXT[] NOT NULL,
    -- Echoed back to the client untouched, so it can match the response to
    -- the request it made.
    state TEXT,
    code_challenge TEXT NOT NULL,
    -- The RFC 8707 resource the tokens will be bound to.
    resource TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- One person's consent for one client: what a "connected app" is.
--
-- Every token descends from a grant, so revoking the grant ends the
-- connection whole. A grant is also the refresh-token family: presenting a
-- refresh token that was already rotated away revokes the grant it belongs
-- to.
CREATE TABLE oauth_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id UUID NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    scopes TEXT[] NOT NULL,
    resource TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When a token from this grant was last issued, which is what the account
    -- screen reads as "last used".
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Set, never deleted, so the account screen and the audit log agree about
    -- what was ended and when.
    revoked_at TIMESTAMPTZ
);

CREATE INDEX oauth_grants_user_id ON oauth_grants (user_id);
CREATE INDEX oauth_grants_client_id ON oauth_grants (client_id);

-- A single-use authorization code, exchanged once for the first tokens.
CREATE TABLE oauth_authorization_codes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grant_id UUID NOT NULL REFERENCES oauth_grants(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    redirect_uri TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    -- Set by the exchange that spent it. A second exchange finds it set, which
    -- is a replayed code, and revokes the grant.
    used_at TIMESTAMPTZ
);

CREATE INDEX oauth_authorization_codes_grant_id ON oauth_authorization_codes (grant_id);

-- Refresh tokens. Each one is spent by the refresh that rotates it.
CREATE TABLE oauth_refresh_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grant_id UUID NOT NULL REFERENCES oauth_grants(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ
);

CREATE INDEX oauth_refresh_tokens_grant_id ON oauth_refresh_tokens (grant_id);

-- Short-lived access tokens, presented as bearer tokens to the resource.
CREATE TABLE oauth_access_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grant_id UUID NOT NULL REFERENCES oauth_grants(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    -- What this token may do: the grant's scopes, or fewer when a refresh
    -- asked for less. Never more, which the token endpoint enforces.
    scopes TEXT[] NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX oauth_access_tokens_grant_id ON oauth_access_tokens (grant_id);
