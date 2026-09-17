DROP INDEX users_platform_roles_held;

ALTER TABLE users
    DROP COLUMN platform_roles;
