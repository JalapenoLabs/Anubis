// Copyright © 2026 Jalapeno Labs

import type { RealtimeListener } from '../realtime/listeners'

// Core
import { useEffect, useRef } from 'react'

// Misc
import { EventStream } from '../realtime/EventStream'

/**
 * Listens to an application's own event stream for as long as the component is mounted.
 *
 * For the streams an application serves from its own endpoints with
 * `Channels::stream`, such as a game server's log tail at
 * `/api/v1/servers/{id}/logs`. Each is its own connection, resuming with
 * `Last-Event-ID` after a drop exactly as `/realtime` does, and closing when
 * the component unmounts or the URL changes.
 *
 * Pass `null` as the URL while it is not known yet; nothing is opened until
 * it is. The handler may be a new function on every render without
 * reconnecting, so it can close over fresh props.
 *
 * ```tsx
 * useEventStream(`/api/v1/servers/${serverId}/logs`, (message) => {
 *   if (message.type === 'reset') {
 *     clearLines()
 *     return
 *   }
 *   appendLine(message.payload)
 * })
 * ```
 */
export function useEventStream(url: string | null, onMessage: RealtimeListener): void {
  const handler = useRef(onMessage)

  useEffect(() => {
    handler.current = onMessage
  }, [ onMessage ])

  useEffect(() => {
    if (!url) {
      console.debug('useEventStream was given no URL, so nothing is opened')
      return () => {}
    }

    const stream = new EventStream({
      url,
      onFrame: (frame) => {
        if (frame.type === 'event' || frame.type === 'reset') {
          handler.current(frame)
          return
        }
        if (frame.type === 'error') {
          console.warn('The event stream refused a channel', frame)
        }
      },
    })

    return () => stream.close()
  }, [ url ])
}
