ALTER TABLE user_tokens
    DROP COLUMN attempts,
    DROP COLUMN payload;

ALTER TABLE users
    DROP COLUMN locale,
    DROP COLUMN time_zone,
    DROP COLUMN last_name,
    DROP COLUMN first_name;
