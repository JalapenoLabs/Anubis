// Copyright © 2026 Jalapeno Labs

import type { User } from '../api/types'

// Core
import useSWR from 'swr'

// Utility
import { HTTPError } from 'ky'

// Misc
import { ApiErrorCode, getApiErrorCode } from '../api/errors'
import { useAnubisApi } from './AnubisProvider'

const CURRENT_USER_KEY = 'anubis/current-user'

type CurrentUserState = {
  user: User | null
  passwordChangeRequired: boolean
}

/**
 * The signed-in user, or null when signed out.
 *
 * A 401 from the backend is the normal signed-out state, not an error; only
 * transport and server failures surface through `error`. Call `refresh` after
 * a login, logout, or profile change so every subscriber updates.
 *
 * An account an administrator told to choose a new password is signed in and
 * still has no user to show, because every route but the password change
 * refuses it. That state is `passwordChangeRequired`, and an app routes to its
 * forced-change screen on it rather than treating the account as signed out.
 */
export function useCurrentUser() {
  const api = useAnubisApi()

  const { data, error, isLoading, mutate } = useSWR<CurrentUserState>(
    CURRENT_USER_KEY,
    async () => {
      try {
        return {
          user: await api.me(),
          passwordChangeRequired: false,
        }
      }
      catch (caught) {
        if (caught instanceof HTTPError && caught.response.status === 401) {
          return {
            user: null,
            passwordChangeRequired: false,
          }
        }
        if (getApiErrorCode(caught) === ApiErrorCode.passwordChangeRequired) {
          return {
            user: null,
            passwordChangeRequired: true,
          }
        }
        throw caught
      }
    },
  )

  return {
    user: data?.user ?? null,
    passwordChangeRequired: data?.passwordChangeRequired ?? false,
    isLoading,
    error,
    refresh: mutate,
  } as const
}

export type CurrentUserResult = ReturnType<typeof useCurrentUser>
