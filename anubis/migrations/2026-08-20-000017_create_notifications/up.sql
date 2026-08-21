-- In-app notifications: one row per person told about one thing.
--
-- Written through the caller's own connection, so a notification commits with
-- the domain write that caused it, exactly as a webhook delivery does. The row
-- is the durable half; the realtime ping that follows carries nothing and is
-- only a signal to refetch. See `anubis::notifications`.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Who reads it. Deleting the account takes the inbox with it.
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The team the notice is about, when it is about one. Null for anything
    -- addressed to the person rather than to their standing in a team.
    team_id UUID REFERENCES teams(id) ON DELETE CASCADE,
    -- A machine-readable type, `<subject>.<event>`, e.g. `invitation.received`.
    -- It is an identifier rather than prose: an application that translates its
    -- inbox looks the copy up by this and ignores the stored text.
    kind TEXT NOT NULL,
    -- What the bell shows, already rendered. Notifications are written once and
    -- read as written, so the text is stored rather than recomputed from a
    -- record that may since have changed or been deleted.
    title TEXT NOT NULL,
    body TEXT,
    -- Where the entry navigates, as an application path such as
    -- `/teams/{id}/settings`. Null for a notice with nowhere to go.
    href TEXT,
    -- Null until the recipient reads it, which is also what the badge counts.
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The inbox: one user's notifications, newest first. Unread-first ordering is
-- a second key on top of this one, which Postgres sorts after the index has
-- already narrowed the rows to one recipient's page.
CREATE INDEX notifications_recipient_idx
    ON notifications (user_id, created_at DESC);

-- The badge: how many of one user's notifications are unread. Partial, because
-- an inbox is mostly read and the count is asked for on every page load.
CREATE INDEX notifications_unread_idx
    ON notifications (user_id)
    WHERE read_at IS NULL;
