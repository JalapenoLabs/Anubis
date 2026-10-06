ALTER TABLE audit_events
    DROP COLUMN reported_location,
    DROP COLUMN user_agent,
    DROP COLUMN ip_address;
