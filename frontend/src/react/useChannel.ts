// Copyright © 2026 Jalapeno Labs

import type { RealtimeListener } from '../realtime/listeners'

// Core
import { useEffect, useRef } from 'react'

// Misc
import { useRealtime } from './RealtimeProvider'

/**
 * Listens to a realtime channel for as long as the component is mounted.
 *
 * Pass `null` as the channel while the name is not known yet, such as before
 * the current team has loaded; nothing is subscribed until it is. The handler
 * may be a new function on every render without resubscribing, so it can close
 * over fresh props.
 *
 * ```tsx
 * useChannel(`team:${teamId}:projects`, (event) => {
 *   if (event.event === 'created') {
 *     void mutate('projects')
 *   }
 * })
 * ```
 *
 * Events published while the socket was down are not replayed, so a view that
 * must be exact should refetch rather than apply events to local state alone.
 */
export function useChannel(channel: string | null, onEvent: RealtimeListener): void {
  const client = useRealtime()
  const handler = useRef(onEvent)

  useEffect(() => {
    handler.current = onEvent
  }, [ onEvent ])

  useEffect(() => {
    if (!channel) {
      console.debug('useChannel was given no channel, so nothing is subscribed')
      return () => {}
    }

    return client.subscribe(channel, (event) => handler.current(event))
  }, [ client, channel ])
}
