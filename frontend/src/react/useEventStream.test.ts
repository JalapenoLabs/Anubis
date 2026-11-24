// Copyright © 2026 Jalapeno Labs

import type { RealtimeMessage } from '../realtime/protocol'

// Core
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Utility
import { renderHook, waitFor } from '@testing-library/react'

// Misc
import { useEventStream } from './useEventStream'

const URL_PATH = 'http://localhost/api/v1/servers/7/logs'

/** One stream the hook opened, as the fake server sees it. */
type Connection = {
  url: string
  aborted: boolean
  push: (text: string) => void
}

type Context = {
  connections: Connection[]
}

/** The raw text of one frame, with an id when it carries one. */
function frame(data: unknown, id?: string): string {
  const idLine = id
    ? `id: ${id}\n`
    : ''
  return `${idLine}data: ${JSON.stringify(data)}\n\n`
}

describe('useEventStream', () => {
  beforeEach<Context>((context) => {
    context.connections = []
    vi.stubGlobal('fetch', async (input: Request | string | URL, init?: RequestInit) => {
      const request = input instanceof Request
        ? input
        : new Request(input, init)
      const encoder = new TextEncoder()
      let controller: ReadableStreamDefaultController<Uint8Array> | null = null
      const body = new ReadableStream<Uint8Array>({
        start(start) {
          controller = start
        },
      })

      const connection: Connection = {
        url: request.url,
        aborted: false,
        push: (text) => controller?.enqueue(encoder.encode(text)),
      }
      request.signal.addEventListener('abort', () => {
        connection.aborted = true
        controller?.error(new DOMException('aborted', 'AbortError'))
      })
      context.connections.push(connection)

      return new Response(body, {
        status: 200,
        headers: { 'content-type': 'text/event-stream' },
      })
    })
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it<Context>('should hand the handler each event and reset, and close on unmount', async (context) => {
    const heard: RealtimeMessage[] = []
    const { unmount } = renderHook(() => useEventStream(URL_PATH, (message) => heard.push(message)))

    await waitFor(() => {
      expect(context.connections).toHaveLength(1)
    })
    const connection = context.connections[0]
    expect(connection.url).toBe(URL_PATH)

    connection.push(frame({ type: 'reset', channel: 'resource:r:logs' }))
    connection.push(frame({ type: 'live' }, 'e-1'))
    connection.push(frame({
      type: 'event',
      channel: 'resource:r:logs',
      event: 'line',
      payload: { text: 'server started' },
    }, 'e-2'))

    await waitFor(() => {
      expect(heard).toHaveLength(2)
    })
    expect(heard).toEqual([
      { type: 'reset', channel: 'resource:r:logs' },
      {
        type: 'event',
        id: 'e-2',
        channel: 'resource:r:logs',
        event: 'line',
        payload: { text: 'server started' },
      },
    ])

    unmount()
    expect(connection.aborted).toBe(true)
  })

  it<Context>('should open nothing until it has a URL', async (context) => {
    type Props = { url: string | null }
    const initialProps: Props = { url: null }
    const { rerender } = renderHook(
      (props: Props) => useEventStream(props.url, () => {}),
      { initialProps },
    )

    // Long enough for a stream that was going to open to have asked.
    await new Promise((resolve) => setTimeout(resolve, 20))
    expect(context.connections).toHaveLength(0)

    rerender({ url: URL_PATH })
    await waitFor(() => {
      expect(context.connections).toHaveLength(1)
    })
  })
})
