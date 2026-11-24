-- A device request has no redirect URI, so it cannot survive the columns
-- becoming required again.
DELETE FROM oauth_authorization_requests WHERE device_code_id IS NOT NULL;

ALTER TABLE oauth_authorization_requests
    DROP CONSTRAINT oauth_authorization_requests_delivery,
    DROP COLUMN entered_by,
    DROP COLUMN device_code_id,
    ALTER COLUMN redirect_uri SET NOT NULL,
    ALTER COLUMN code_challenge SET NOT NULL;

DROP TABLE oauth_device_codes;

ALTER TABLE oauth_clients DROP COLUMN grant_types;
