// Copyright © 2026 Jalapeno Labs

import type { RealtimeMessage } from '../realtime/protocol'
import type { ReactNode } from 'react'

// Core
import { afterEach, describe, expect, it } from 'vitest'

// Utility
import { renderHook, waitFor } from '@testing-library/react'

// Misc
import { RealtimeClient } from '../realtime/RealtimeClient'
import { RealtimeProvider } from './RealtimeProvider'
import { useChannel } from './useChannel'

const PROJECTS = 'team:11111111-1111-1111-1111-111111111111:projects'

/**
 * A fetch whose every stream opens the way the server opens a fresh channel:
 * a reset, then `live`, and then nothing until it is closed.
 */
function createFetch(requests: string[]) {
  return async (input: Request | string | URL, init?: RequestInit): Promise<Response> => {
    const request = input instanceof Request
      ? input
      : new Request(input, init)
    requests.push(request.url)

    const opening = `data: ${JSON.stringify({ type: 'reset', channel: PROJECTS })}\n\n`
      + `id: e-1\ndata: ${JSON.stringify({ type: 'live' })}\n\n`
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(opening))
        request.signal.addEventListener('abort', () => {
          controller.error(new DOMException('aborted', 'AbortError'))
        })
      },
    })
    return new Response(body, {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    })
  }
}

describe('useChannel', () => {
  let client: RealtimeClient | null = null

  afterEach(() => {
    client?.close()
    client = null
  })

  it('should keep one subscription while its handler is a new function on every render', async () => {
    const requests: string[] = []
    client = new RealtimeClient({ url: 'http://localhost/realtime', fetch: createFetch(requests) })
    const provided = client
    const heard: RealtimeMessage[] = []
    function wrapper(props: { children: ReactNode }) {
      return <RealtimeProvider client={provided}>{props.children}</RealtimeProvider>
    }

    const { rerender } = renderHook(
      () => useChannel(PROJECTS, (message) => heard.push(message)),
      { wrapper },
    )
    await waitFor(() => expect(heard).toHaveLength(1))

    // Each render hands over a new inline handler on a channel already live.
    // A resubscription would earn each one a reset, and a view that refetches
    // on a reset would render again, and loop.
    rerender()
    rerender()
    await new Promise((resolve) => setTimeout(resolve, 20))

    expect(heard).toEqual([{ type: 'reset', channel: PROJECTS }])
    expect(requests).toHaveLength(1)
  })
})
