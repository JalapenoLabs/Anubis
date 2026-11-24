// Copyright © 2026 Jalapeno Labs

import type { RealtimeMessage } from './protocol'

// Core
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Misc
import { RealtimeClient } from './RealtimeClient'

const PROJECTS = 'team:11111111-1111-1111-1111-111111111111:projects'
const INBOX = 'user:22222222-2222-2222-2222-222222222222:inbox'
const ALERTS = 'user:22222222-2222-2222-2222-222222222222:alerts'

/** One stream the client opened, as the fake server sees it. */
type Connection = {
  channels: string[]
  fresh: string[]
  lastEventId: string | null
  aborted: boolean
  /** Sends raw event-stream text down the connection. */
  push: (text: string) => void
  /** Ends the stream cleanly, as the server does every few minutes. */
  end: () => void
}

type Context = {
  connections: Connection[]
  /** The status the fake server refuses every request with, or none. */
  refusal: number | null
  client: RealtimeClient
}

/**
 * A fetch that answers every request with a stream the test drives, unless
 * the test has the server refusing them. Every request is recorded either way.
 */
function createFetch(context: Context) {
  const connections = context.connections
  return async (input: Request | string | URL, init?: RequestInit): Promise<Response> => {
    const request = input instanceof Request
      ? input
      : new Request(input, init)
    const query = new URL(request.url).searchParams

    const encoder = new TextEncoder()
    let controller: ReadableStreamDefaultController<Uint8Array> | null = null
    const body = new ReadableStream<Uint8Array>({
      start(start) {
        controller = start
      },
    })

    const connection: Connection = {
      channels: (query.get('channels') ?? '').split(',').filter(Boolean),
      fresh: (query.get('fresh') ?? '').split(',').filter(Boolean),
      lastEventId: request.headers.get('last-event-id'),
      aborted: false,
      push: (text) => controller?.enqueue(encoder.encode(text)),
      end: () => controller?.close(),
    }
    request.signal.addEventListener('abort', () => {
      connection.aborted = true
      controller?.error(new DOMException('aborted', 'AbortError'))
    })
    connections.push(connection)
    if (context.refusal) {
      return new Response('{"message":"refused"}', {
        status: context.refusal,
        headers: { 'content-type': 'application/json' },
      })
    }

    return new Response(body, {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    })
  }
}

/** The raw text of one frame, with an id when it carries one. */
function frame(data: unknown, id?: string): string {
  const idLine = id
    ? `id: ${id}\n`
    : ''
  return `${idLine}data: ${JSON.stringify(data)}\n\n`
}

/** Lets coalesced rebuilds, the stream's promises, and due timers run. */
async function settle(milliseconds = 0) {
  await vi.advanceTimersByTimeAsync(milliseconds)
}

/** The connection the client is using now. */
function current(context: Context): Connection {
  const connection = context.connections.at(-1)
  if (!connection) {
    throw new Error('the client has opened no stream')
  }
  return connection
}

describe('RealtimeClient', () => {
  beforeEach<Context>((context) => {
    vi.useFakeTimers()
    context.connections = []
    context.refusal = null
    context.client = new RealtimeClient({
      url: 'http://localhost/realtime',
      fetch: createFetch(context),
    })
  })

  afterEach<Context>((context) => {
    context.client.close()
    vi.useRealTimers()
  })

  it<Context>('should open one stream for every channel subscribed together', async (context) => {
    context.client.subscribe(PROJECTS, () => {})
    context.client.subscribe(INBOX, () => {})
    context.client.subscribe(PROJECTS, () => {})
    expect(context.connections).toHaveLength(0)

    await settle()

    expect(context.connections).toHaveLength(1)
    expect(current(context).channels).toEqual([ PROJECTS, INBOX ].sort())
    // A first connection has no cursor, so every channel is fresh unsaid.
    expect(current(context).fresh).toEqual([])
    expect(current(context).lastEventId).toBeNull()
    expect(context.client.state).toBe('open')
  })

  it<Context>('should route each message to the listeners of its channel', async (context) => {
    const projects: RealtimeMessage[] = []
    const inbox: RealtimeMessage[] = []
    context.client.subscribe(PROJECTS, (message) => projects.push(message))
    context.client.subscribe(INBOX, (message) => inbox.push(message))
    await settle()

    current(context).push(frame({ type: 'reset', channel: PROJECTS }))
    current(context).push(frame({ type: 'live' }, 'e-1'))
    current(context).push(frame({
      type: 'event',
      channel: INBOX,
      event: 'created',
      payload: { unread: 2 },
    }, 'e-2'))
    await settle()

    expect(projects).toEqual([{ type: 'reset', channel: PROJECTS }])
    expect(inbox).toEqual([
      { type: 'event', id: 'e-2', channel: INBOX, event: 'created', payload: { unread: 2 }},
    ])
  })

  it<Context>('should resume what was live and open a new channel fresh when the set changes', async (context) => {
    context.client.subscribe(PROJECTS, () => {})
    await settle()
    const first = current(context)
    first.push(frame({ type: 'live' }, 'e-4'))
    first.push(frame({ type: 'event', channel: PROJECTS, event: 'x', payload: null }, 'e-5'))
    await settle()

    context.client.subscribe(INBOX, () => {})
    await settle()

    expect(first.aborted).toBe(true)
    const second = current(context)
    expect(second.channels).toEqual([ PROJECTS, INBOX ].sort())
    expect(second.lastEventId).toBe('e-5')
    // The inbox was not on the connection the cursor came from.
    expect(second.fresh).toEqual([ INBOX ])
  })

  it<Context>('should resume every channel once the stream confirmed them live', async (context) => {
    context.client.subscribe(PROJECTS, () => {})
    await settle()
    current(context).push(frame({ type: 'live' }, 'e-1'))
    await settle()
    context.client.subscribe(INBOX, () => {})
    await settle()
    current(context).push(frame({ type: 'live' }, 'e-2'))
    current(context).end()
    await settle(500)

    const resumed = current(context)
    expect(context.connections).toHaveLength(3)
    expect(resumed.lastEventId).toBe('e-2')
    expect(resumed.fresh).toEqual([])
  })

  it<Context>('should keep a channel that left and came back off the old cursor', async (context) => {
    context.client.subscribe(PROJECTS, () => {})
    const stopAlerts = context.client.subscribe(ALERTS, () => {})
    await settle()
    current(context).push(frame({ type: 'live' }, 'e-1'))
    await settle()

    stopAlerts()
    await settle()
    context.client.subscribe(ALERTS, () => {})
    await settle()

    expect(current(context).fresh).toEqual([ ALERTS ])
  })

  it<Context>('should close the stream when the last listener leaves, and reopen for the next', async (context) => {
    const stop = context.client.subscribe(PROJECTS, () => {})
    await settle()
    current(context).push(frame({ type: 'live' }, 'e-1'))
    await settle()

    stop()
    await settle()
    expect(current(context).aborted).toBe(true)
    expect(context.client.state).toBe('idle')

    context.client.subscribe(PROJECTS, () => {})
    await settle()
    expect(context.connections).toHaveLength(2)
    // It left, so it comes back at the head rather than on a stale cursor.
    expect(current(context).fresh).toEqual([ PROJECTS ])
  })

  it<Context>('should not open a new stream when the channels did not change', async (context) => {
    const stop = context.client.subscribe(PROJECTS, () => {})
    await settle()

    // A second listener on a channel already carried, and one leaving again.
    const stopSecond = context.client.subscribe(PROJECTS, () => {})
    stopSecond()
    await settle()

    expect(context.connections).toHaveLength(1)
    stop()
  })

  it<Context>('should not resume a refused channel on the cursor once it is admitted', async (context) => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    context.client.subscribe(PROJECTS, () => {})
    context.client.subscribe(INBOX, () => {})
    await settle()

    // The membership commit is not visible yet, so the team channel is refused.
    current(context).push(frame({
      type: 'error',
      channel: PROJECTS,
      code: 'not_found',
      message: 'No such channel.',
    }))
    current(context).push(frame({ type: 'live' }, 'e-1'))
    current(context).end()
    await settle(500)

    // The reconnect asks for it fresh: it never went live on this cursor.
    expect(current(context).lastEventId).toBe('e-1')
    expect(current(context).fresh).toEqual([ PROJECTS ])
  })

  it<Context>('should reset a listener that joins a channel already live, and only that one', async (context) => {
    const first: RealtimeMessage[] = []
    context.client.subscribe(PROJECTS, (message) => first.push(message))
    await settle()
    current(context).push(frame({ type: 'reset', channel: PROJECTS }))
    current(context).push(frame({ type: 'live' }, 'e-1'))
    await settle()

    const late: RealtimeMessage[] = []
    context.client.subscribe(PROJECTS, (message) => late.push(message))
    const gone: RealtimeMessage[] = []
    const leave = context.client.subscribe(PROJECTS, (message) => gone.push(message))
    leave()
    await settle()

    expect(late).toEqual([{ type: 'reset', channel: PROJECTS }])
    expect(first).toEqual([{ type: 'reset', channel: PROJECTS }])
    expect(gone).toEqual([])
    expect(context.connections).toHaveLength(1)
  })

  it<Context>('should not ask again for a stream refused for good on unchanged channels', async (context) => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    context.refusal = 401
    context.client.subscribe(PROJECTS, () => {})
    await settle()
    expect(context.connections).toHaveLength(1)
    expect(context.client.state).toBe('closed')

    // Components mounting and unmounting on the channel already refused.
    const leave = context.client.subscribe(PROJECTS, () => {})
    leave()
    await settle(60_000)
    expect(context.connections).toHaveLength(1)

    // A different set of channels is a different request, and is asked.
    context.refusal = null
    context.client.subscribe(INBOX, () => {})
    await settle()
    expect(context.connections).toHaveLength(2)
    expect(context.client.state).toBe('open')
  })

  it<Context>('should forget what an earlier connection refused once a new one opens', async (context) => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    context.client.subscribe(PROJECTS, () => {})
    context.client.subscribe(INBOX, () => {})
    await settle()

    // Refused, and then the connection drops before it went live.
    current(context).push(frame({
      type: 'error',
      channel: PROJECTS,
      code: 'not_found',
      message: 'No such channel.',
    }))
    current(context).end()
    await settle(500)

    // The stream reconnects on its own, and this time admits both.
    current(context).push(frame({ type: 'live' }, 'e-1'))
    current(context).end()
    await settle(1000)

    expect(context.connections).toHaveLength(3)
    expect(current(context).lastEventId).toBe('e-1')
    expect(current(context).fresh).toEqual([])
  })

  it<Context>('should warn about a channel the server refused', async (context) => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    context.client.subscribe(PROJECTS, () => {})
    await settle()

    current(context).push(frame({
      type: 'error',
      channel: PROJECTS,
      code: 'not_found',
      message: 'No such channel.',
    }))
    await settle()

    expect(warn).toHaveBeenCalledWith(
      'The realtime server refused a channel',
      expect.objectContaining({ code: 'not_found' }),
    )
  })

  it<Context>('should report a disposer called twice without failing', async (context) => {
    const stop = context.client.subscribe(PROJECTS, () => {})
    await settle()

    stop()
    stop()
    await settle()

    expect(context.client.state).toBe('idle')
  })
})
