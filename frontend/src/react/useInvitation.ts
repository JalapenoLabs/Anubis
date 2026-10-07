// Copyright © 2026 Jalapeno Labs

import type { InvitationAcceptance, InvitationPreview, User } from '../api/types'

// Core
import { useCallback } from 'react'
import useSWR, { useSWRConfig } from 'swr'

// Utility
import { HTTPError } from 'ky'

// Misc
import { useAnubisApi } from './AnubisProvider'
import { CURRENT_USER_KEY } from './useCurrentUser'

const INVITATION_KEY = 'anubis/invitation'

/** The status the backend answers every unusable invitation link with. */
const UNUSABLE_LINK_STATUS = 400

/**
 * An operator's invitation, read from the link's token, and the way to accept it.
 *
 * The application renders the page: this reads the token it was handed (from
 * `?token=` in the link, typically), answers the address the invitation was
 * sent to, and accepts it with the password the person chooses. Accepting
 * signs the new account in and writes it into `useCurrentUser`, so the shell
 * moves on without a refetch.
 *
 * `isInvalid` covers a missing token and every link the backend refuses,
 * which it does identically for unknown, used, revoked, and expired ones, so
 * a page shows one "ask for a new link" state for all of them. `error` is
 * left for everything else, a failed network or server, which is worth a
 * retry rather than a new link.
 */
export function useInvitation(token: string | null) {
  const api = useAnubisApi()
  const { mutate } = useSWRConfig()

  const { data, error, isLoading } = useSWR<InvitationPreview>(
    token
      ? [ INVITATION_KEY, token ]
      : null,
    () => api.lookupInvitation(token ?? ''),
    {
      // A refused link stays refused, and a token is spent by accepting it, so
      // neither a retry nor a refetch on focus can tell the page anything new.
      shouldRetryOnError: false,
      revalidateOnFocus: false,
    },
  )

  const isRefused = error instanceof HTTPError
    && error.response.status === UNUSABLE_LINK_STATUS

  const accept = useCallback(async (acceptance: InvitationAcceptance): Promise<User> => {
    if (!token) {
      throw new Error('useInvitation cannot accept without a token')
    }

    const user = await api.acceptInvitation(token, acceptance)
    await mutate(CURRENT_USER_KEY, user, { revalidate: false })
    return user
  }, [ api, mutate, token ])

  return {
    invitation: data ?? null,
    isLoading,
    isInvalid: !token || isRefused,
    error: isRefused
      ? undefined
      : error,
    accept,
  } as const
}

export type InvitationResult = ReturnType<typeof useInvitation>
