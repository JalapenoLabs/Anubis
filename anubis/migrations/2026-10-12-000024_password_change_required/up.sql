-- Whether the account must choose a new password before it may do anything
-- else.
--
-- Set when an operator gives the account a temporary password, and cleared by
-- the first password the account chooses for itself. While it is set, the
-- account signs in normally and every authenticated route but the few that
-- change the password, sign out, and read the account answers `403`.
ALTER TABLE users
    ADD COLUMN password_change_required BOOLEAN NOT NULL DEFAULT false;
