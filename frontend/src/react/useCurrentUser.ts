// Copyright © 2026 Jalapeno Labs

import type { User } from '../api/types'

// Core
import useSWR from 'swr'

// Utility
import { HTTPError } from 'ky'

// Misc
import { useAnubisApi } from './AnubisProvider'

const CURRENT_USER_KEY = 'anubis/current-user'

/**
 * The signed-in user, or null when signed out.
 *
 * A 401 from the backend is the normal signed-out state, not an error; only
 * transport and server failures surface through `error`. Call `refresh` after
 * a login, logout, or profile change so every subscriber updates.
 */
export function useCurrentUser() {
  const api = useAnubisApi()

  const { data, error, isLoading, mutate } = useSWR<User | null>(
    CURRENT_USER_KEY,
    async () => {
      try {
        return await api.me()
      }
      catch (caught) {
        if (caught instanceof HTTPError && caught.response.status === 401) {
          return null
        }
        throw caught
      }
    },
  )

  return {
    user: data ?? null,
    isLoading,
    error,
    refresh: mutate,
  } as const
}

export type CurrentUserResult = ReturnType<typeof useCurrentUser>
