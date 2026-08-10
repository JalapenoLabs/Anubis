ALTER TABLE users
    ADD COLUMN first_name TEXT,
    ADD COLUMN last_name TEXT,
    ADD COLUMN time_zone TEXT NOT NULL DEFAULT 'UTC',
    ADD COLUMN locale TEXT NOT NULL DEFAULT 'en-US';

ALTER TABLE user_tokens
    -- Carries flow-specific data, e.g. the new address for an email change.
    ADD COLUMN payload TEXT,
    -- Counts failed verification attempts for guessable secrets (sign-in codes).
    ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
