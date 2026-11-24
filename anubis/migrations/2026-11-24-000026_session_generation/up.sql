-- Which generation of sessions the account accepts.
--
-- Every act that signs an account out everywhere (a password reset, a
-- temporary password, a password change for every browser but the one making
-- it) advances this in the same transaction as the credential change. A
-- session signs nobody in unless it carries the current value, so ending them
-- all is decided here, in Postgres, and commits with the change that caused
-- it, whichever store holds the sessions themselves.
ALTER TABLE users
    ADD COLUMN session_generation BIGINT NOT NULL DEFAULT 0;

-- The account's session generation when this session was created, or when a
-- password change kept it. A session whose generation is no longer the
-- account's is ended, whether or not its row is still here.
ALTER TABLE sessions
    ADD COLUMN generation BIGINT NOT NULL DEFAULT 0;
