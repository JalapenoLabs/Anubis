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
 * The handler hears each event, and a `reset` whenever what the view holds may
 * be stale: once when the channel goes live, and again if a reconnect spanned
 * more than the server keeps. Events missed during a short drop are replayed
 * in order, so a view may write payloads straight into its cache as long as it
 * revalidates on a reset:
 *
 * ```tsx
 * useChannel(`team:${teamId}:projects`, (message) => {
 *   if (message.type === 'reset') {
 *     void mutate()
 *     return
 *   }
 *   void mutate((page) => applyProjectEvent(page, message), { revalidate: false })
 * })
 * ```
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
