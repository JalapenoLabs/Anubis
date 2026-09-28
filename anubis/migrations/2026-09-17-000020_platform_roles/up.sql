-- Roles held at the platform tier: the deployment itself, above every
-- organization in it.
--
-- The column lives on the user rather than in a membership table because the
-- platform has no roster to join through. There is one platform, it is the
-- thing the process is serving, and a person either operates it or does not.
ALTER TABLE users
    ADD COLUMN platform_roles TEXT[] NOT NULL DEFAULT '{}';

-- Operators are a handful of people among every account, so the index only
-- carries the rows that hold a role. "Who operates this deployment?" is the
-- one query that reads the column without already knowing the user.
CREATE INDEX users_platform_roles_held
    ON users USING GIN (platform_roles)
    WHERE platform_roles <> '{}';
