// Copyright © 2026 Jalapeno Labs

import type { RealtimeClient } from '../realtime/RealtimeClient'
import type { ReactNode } from 'react'

// Core
import { createContext, useContext } from 'react'

type Props = {
  client: RealtimeClient
  children: ReactNode
}

const RealtimeContext = createContext<RealtimeClient | null>(null)

/**
 * Provides one realtime connection to the component tree.
 *
 * Mount once near the root, above anything using `useChannel`. One client is
 * one websocket however many channels the page watches, so building a second
 * provider means opening a second socket.
 */
export function RealtimeProvider(props: Props) {
  return <RealtimeContext.Provider value={props.client}>{
      props.children
    }</RealtimeContext.Provider>
}

/** Returns the client provided by the nearest RealtimeProvider. */
export function useRealtime(): RealtimeClient {
  const client = useContext(RealtimeContext)
  if (!client) {
    throw new Error('useRealtime requires a <RealtimeProvider> above it in the tree')
  }
  return client
}
