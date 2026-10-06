-- Where each recorded act came from: the client's address, its browser, and,
-- behind a trusted load balancer, its approximate location.
--
-- Copied from the request onto the row for the same reason `actor_name` is:
-- the log has to answer "where did this sign-in come from" long after the
-- request that could have said so is gone. All three are nullable, because an
-- act a job or a sweep performed had no client, and a deployment that names
-- no trusted location header records no location.
--
-- Plain text rather than `inet`: the column is read by people, not compared
-- by the database, and text keeps the framework's schema free of a type every
-- application's Diesel setup would then have to map.
ALTER TABLE audit_events
    ADD COLUMN ip_address TEXT,
    ADD COLUMN user_agent TEXT,
    ADD COLUMN location TEXT;
