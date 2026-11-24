// Copyright © 2026 Jalapeno Labs

import type { RoleCatalog } from '../api/types'

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

/**
 * Where a catalog is read from, which decides whose standing it carries.
 *
 * An organization's own screens read it organization-wide; a team's screens
 * read it through the team, which is also the only way a member of the team
 * alone can see the roles they may grant there.
 */
export type RoleCatalogScope =
  | { organizationId: string }
  | { teamId: string }

/**
 * An organization's roles, the vocabulary they draw from, and the viewer's
 * standing where they asked.
 *
 * Call `refresh` after any change to a role, a grant, or the order, so every
 * screen naming a role redraws it.
 */
export function useRoles(scope: RoleCatalogScope | null) {
  const api = useAnubisApi()

  let key: string | null = null
  if (scope && 'organizationId' in scope) {
    key = `anubis/roles/organization/${scope.organizationId}`
  }
  else if (scope) {
    key = `anubis/roles/team/${scope.teamId}`
  }

  const { data, error, isLoading, mutate } = useSWR<RoleCatalog>(key, () => {
    if (scope && 'organizationId' in scope) {
      return api.listOrganizationRoles(scope.organizationId)
    }
    if (scope) {
      return api.listTeamRoles(scope.teamId)
    }
    throw new Error('useRoles fetched with no scope')
  })

  return {
    catalog: data,
    isLoading,
    error,
    refresh: mutate,
  } as const
}

export type RolesResult = ReturnType<typeof useRoles>
