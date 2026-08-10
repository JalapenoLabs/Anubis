-- Avatars live apart from users so the hot users row (loaded on every
-- authenticated request) never carries image bytes.
CREATE TABLE user_avatars (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    image BYTEA NOT NULL,
    content_type TEXT NOT NULL,
    etag TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
