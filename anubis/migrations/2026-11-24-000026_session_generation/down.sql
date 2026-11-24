ALTER TABLE sessions
    DROP COLUMN generation;

ALTER TABLE users
    DROP COLUMN session_generation;

DROP SEQUENCE session_generations;
