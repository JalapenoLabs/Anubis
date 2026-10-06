// Copyright © 2026 Jalapeno Labs

import type { ConnectedClient } from '../api/types'

// Core
import { useCallback } from 'react'
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'
import { useCurrentUser } from './useCurrentUser'

const CONNECTED_CLIENTS_KEY = 'anubis/connected-clients'

/**
 * The programs the signed-in person connected, such as Claude Code, and the
 * one action they have over them: revoking.
 *
 * Nothing is fetched while signed out. `revoke` ends the connection on the
 * server first and refetches after, so the list never shows a client as gone
 * while its tokens still work.
 */
export function useConnectedClients() {
  const api = useAnubisApi()
  const { user } = useCurrentUser()

  const { data, error, isLoading, mutate } = useSWR<ConnectedClient[]>(
    user ? CONNECTED_CLIENTS_KEY : null,
    () => api.listConnectedClients(),
  )

  const revoke = useCallback(async (connectionId: string) => {
    await api.revokeConnectedClient(connectionId)
    await mutate()
  }, [ api, mutate ])

  return {
    connectedClients: data ?? [],
    isLoading,
    error,
    revoke,
  } as const
}

export type ConnectedClientsResult = ReturnType<typeof useConnectedClients>
