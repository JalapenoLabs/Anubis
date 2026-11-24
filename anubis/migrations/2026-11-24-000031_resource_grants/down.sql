DROP TABLE resource_grants;

-- The permission that managed them leaves the vocabulary with the table. A
-- stored key the vocabulary no longer holds already grants nothing, so this
-- only keeps an edited role from carrying a key no screen can show.
UPDATE roles
SET permissions = array_remove(permissions, 'Tenancy::ResourceGrant.update')
WHERE 'Tenancy::ResourceGrant.update' = ANY (permissions);
