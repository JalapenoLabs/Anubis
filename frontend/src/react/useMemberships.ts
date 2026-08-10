// Copyright © 2026 Jalapeno Labs

import type { MembershipsOverview } from '../api/types'

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

const MEMBERSHIPS_KEY = 'anubis/memberships'

/**
 * The organizations and teams the signed-in user belongs to.
 *
 * Call `refresh` after claiming an invitation or changing membership so the
 * switcher and every other subscriber update.
 */
export function useMemberships() {
  const api = useAnubisApi()

  const { data, error, isLoading, mutate } = useSWR<MembershipsOverview>(
    MEMBERSHIPS_KEY,
    () => api.listMemberships(),
  )

  return {
    organizations: data?.organizations ?? [],
    isLoading,
    error,
    refresh: mutate,
  } as const
}

export type MembershipsResult = ReturnType<typeof useMemberships>
