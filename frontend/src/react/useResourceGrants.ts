// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantsOverview } from '../api/types'

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

/**
 * Which teams reach the records an organization owns directly, as the viewer
 * may read it.
 *
 * Pass `null` until the organization is known. Call `refresh` after a change,
 * so the screen reads back what the server kept rather than what it sent.
 */
export function useResourceGrants(organizationId: string | null) {
  const api = useAnubisApi()
  const key = organizationId
    ? `anubis/resource-grants/${organizationId}`
    : null

  const { data, error, isLoading, mutate } = useSWR<ResourceGrantsOverview>(key, () => {
    if (!organizationId) {
      throw new Error('useResourceGrants fetched with no organization')
    }
    return api.listResourceGrants(organizationId)
  })

  return {
    overview: data,
    isLoading,
    error,
    refresh: mutate,
  } as const
}

export type ResourceGrantsResult = ReturnType<typeof useResourceGrants>
