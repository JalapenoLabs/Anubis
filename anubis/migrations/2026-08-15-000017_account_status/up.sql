-- Account status: the two states an administrator can put an account into.
--
-- Offboarding used to mean deleting the user, which takes their authored rows
-- with it, and a provisioned credential had no way to be forced out of use.
-- See `docs/tenancy.md`.
ALTER TABLE users
    -- When an administrator disabled the account, null while it is usable. A
    -- disabled account authenticates against nothing: sign-in refuses, and the
    -- act of disabling deletes the user's sessions rather than letting them run
    -- out the clock.
    ADD COLUMN disabled_at TIMESTAMPTZ,
    -- Whether the account owes a password change before it may do anything
    -- else. Every authenticated route refuses until the change lands, which is
    -- what clears the flag. Existing rows default to false, which is the
    -- behavior every account had before this column existed.
    ADD COLUMN password_change_required BOOLEAN NOT NULL DEFAULT false;
