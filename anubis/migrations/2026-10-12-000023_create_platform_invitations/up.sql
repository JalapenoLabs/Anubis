-- Invitations an operator sends to bring somebody into the deployment.
--
-- A platform invitation is not a team invitation. It names no tenant: it asks
-- a person to open an account on the deployment itself, optionally holding a
-- platform role from the moment the account exists. The account is created
-- when the invitation is accepted, never before, so an address nobody answered
-- for leaves no account behind, no personal organization, and no placeholder
-- password, and every other sign-in path behaves for it exactly as it does for
-- an address nobody has invited.
CREATE TABLE platform_invitations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Normalized (trimmed, lowercased) exactly as `users.email` is, so the
    -- check that no account holds the address compares like with like.
    email TEXT NOT NULL,
    -- The platform role the account is granted when it is created, if any.
    platform_role TEXT,
    invited_by UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Copied, as the audit log copies an actor's name, so the listing still
    -- says who sent an invitation after that operator's account is gone.
    invited_by_name TEXT NOT NULL,
    -- SHA-256 of the emailed token. Replaced on every resend, which is what
    -- makes the previous link stop working.
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the current link went out; a resend moves it.
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    -- Set together when the invitee sets a password and the account is made.
    accepted_at TIMESTAMPTZ,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    revoked_at TIMESTAMPTZ,
    CHECK (accepted_at IS NULL OR revoked_at IS NULL)
);

-- One live invitation per address. A second invite is refused with a sentence
-- that says to resend, and this index is what decides two invites that race.
CREATE UNIQUE INDEX platform_invitations_live_email
    ON platform_invitations (email)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;

-- What an operator's table lists: the live invitations, most recently sent
-- first. Partial, so its size is the number of people still invited rather
-- than everybody who ever was.
CREATE INDEX platform_invitations_live_sent
    ON platform_invitations (sent_at DESC, id DESC)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
