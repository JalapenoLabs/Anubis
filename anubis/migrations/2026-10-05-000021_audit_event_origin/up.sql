-- Where each recorded act came from: the client's address, its browser, and
-- the approximate location its browser reported.
--
-- Copied from the request onto the row for the same reason `actor_name` is:
-- the log has to answer "where did this sign-in come from" long after the
-- request that could have said so is gone. All three are nullable, because an
-- act a job or a sweep performed had no client, and a browser that did not
-- look itself up reported no location.
--
-- `reported_location` is named for who said it. The browser asks a
-- geolocation service and passes the answer on in a header anybody can set,
-- so the column records a claim, never a measurement.
--
-- Plain text rather than `inet`: the column is read by people, not compared
-- by the database, and text keeps the framework's schema free of a type every
-- application's Diesel setup would then have to map.
ALTER TABLE audit_events
    ADD COLUMN ip_address TEXT,
    ADD COLUMN user_agent TEXT,
    ADD COLUMN reported_location TEXT;
