// Copyright © 2026 Jalapeno Labs

import type { StreamFrame } from './protocol'

// Core
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Misc
import { EventStream, reconnectDelay } from './EventStream'

/** One connection the stream opened, as the fake server sees it. */
type Connection = {
  url: string
  lastEventId: string | null
  /** Whether the client cancelled the body, which closes the connection. */
  cancelled: boolean
  /** Sends raw event-stream text down the connection. */
  push: (text: string) => void
  /** Ends the stream cleanly, as the server does every few minutes. */
  end: () => void
  /** Breaks the connection mid-stream. */
  drop: () => void
}

/** How the fake server answers the next connection. */
type Answer = 'stream' | 'network-error' | number | { status: number, retryAfter: string }

type Context = {
  connections: Connection[]
  answers: Answer[]
  frames: StreamFrame[]
  fetch: (input: Request | string | URL, init?: RequestInit) => Promise<Response>
}

/**
 * A fetch that answers every request with a stream the test drives.
 *
 * Each answer is taken from the front of `answers`, so a test can script a
 * refusal or a network failure before the stream it expects next.
 */
function createFetch(context: Context) {
  return async (input: Request | string | URL, init?: RequestInit): Promise<Response> => {
    const request = input instanceof Request
      ? input
      : new Request(input, init)
    const answer = context.answers.shift() ?? 'stream'

    if (answer === 'network-error') {
      throw new TypeError('Failed to fetch')
    }
    if (typeof answer === 'number') {
      return new Response('{"message":"refused"}', {
        status: answer,
        headers: { 'content-type': 'application/json' },
      })
    }
    if (typeof answer === 'object') {
      return new Response('{"message":"wait"}', {
        status: answer.status,
        headers: { 'content-type': 'application/json', 'retry-after': answer.retryAfter },
      })
    }

    const encoder = new TextEncoder()
    let controller: ReadableStreamDefaultController<Uint8Array> | null = null
    const connection: Connection = {
      url: request.url,
      lastEventId: request.headers.get('last-event-id'),
      cancelled: false,
      push: (text) => controller?.enqueue(encoder.encode(text)),
      end: () => controller?.close(),
      drop: () => controller?.error(new TypeError('network error')),
    }
    const body = new ReadableStream<Uint8Array>({
      start(start) {
        controller = start
      },
      cancel() {
        connection.cancelled = true
      },
    })
    request.signal.addEventListener('abort', () => {
      controller?.error(new DOMException('aborted', 'AbortError'))
    })
    context.connections.push(connection)

    return new Response(body, {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    })
  }
}

/** The raw text of one event. */
function event(id: string, data: unknown): string {
  return `id: ${id}\ndata: ${JSON.stringify(data)}\n\n`
}

/** Lets the stream's promises run, and any timer due within `milliseconds`. */
async function settle(milliseconds = 0) {
  await vi.advanceTimersByTimeAsync(milliseconds)
}

describe('reconnectDelay', () => {
  it('should double from half a second', () => {
    expect(reconnectDelay(0)).toBe(500)
    expect(reconnectDelay(1)).toBe(1000)
    expect(reconnectDelay(2)).toBe(2000)
    expect(reconnectDelay(3)).toBe(4000)
    expect(reconnectDelay(4)).toBe(8000)
  })

  it('should never wait longer than ten seconds', () => {
    expect(reconnectDelay(5)).toBe(10_000)
    expect(reconnectDelay(20)).toBe(10_000)
    expect(reconnectDelay(1000)).toBe(10_000)
  })

  it('should treat a nonsense attempt as the first one', () => {
    expect(reconnectDelay(-3)).toBe(500)
  })
})

describe('EventStream', () => {
  beforeEach<Context>((context) => {
    vi.useFakeTimers()
    context.connections = []
    context.answers = []
    context.frames = []
    context.fetch = createFetch(context)
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  function open(context: Context, lastEventId: string | null = null) {
    return new EventStream({
      url: 'http://localhost/stream',
      lastEventId,
      onFrame: (frame) => context.frames.push(frame),
      fetch: context.fetch,
    })
  }

  it<Context>('should deliver what the stream carries, with each event\'s id', async (context) => {
    const stream = open(context)
    await settle()

    context.connections[0].push(`data: ${JSON.stringify({ type: 'reset', channel: 'c' })}\n\n`)
    context.connections[0].push(event('e-2', { type: 'event', channel: 'c', event: 'created', payload: 7 }))
    context.connections[0].push(event('e-3', { type: 'live' }))
    await settle()

    expect(context.frames).toEqual([
      { type: 'reset', channel: 'c' },
      { type: 'event', id: 'e-2', channel: 'c', event: 'created', payload: 7 },
      { type: 'live', id: 'e-3' },
    ])
    expect(stream.state).toBe('open')
    expect(stream.lastEventId).toBe('e-3')
    stream.close()
  })

  it<Context>('should send the id it was given on its first connection', async (context) => {
    const stream = open(context, 'given-7')
    await settle()

    expect(context.connections[0].lastEventId).toBe('given-7')
    stream.close()
  })

  it<Context>('should reconnect at once when a steady stream ends, resuming after the last id', async (context) => {
    const stream = open(context)
    await settle()
    expect(context.connections[0].lastEventId).toBeNull()

    context.connections[0].push(event('e-1', { type: 'live' }))
    await settle(20_000)
    context.connections[0].push(event('e-2', { type: 'event', channel: 'c', event: 'x', payload: null }))
    await settle(20_000)
    context.connections[0].end()
    await settle()

    expect(context.connections).toHaveLength(2)
    expect(context.connections[1].lastEventId).toBe('e-2')
    stream.close()
  })

  it<Context>('should back off from a stream that keeps ending as soon as it opens', async (context) => {
    const stream = open(context)
    await settle()

    // A proxy that forwards the opening and then truncates, every time. The
    // backoff doubles instead of letting the tab reconnect as fast as it can.
    let waited = 0
    for (const delay of [ 500, 1000, 2000 ]) {
      const latest = context.connections.at(-1)
      latest?.push(event(`e-${waited}`, { type: 'live' }))
      latest?.end()
      await settle(delay - 1)
      const before = context.connections.length
      await settle(1)
      expect(context.connections.length).toBe(before + 1)
      waited += delay
    }
    stream.close()
  })

  it<Context>('should back off after a failure, doubling until a connection opens', async (context) => {
    context.answers.push('network-error', 'network-error')
    const stream = open(context)
    await settle()

    // The first failure waits half a second, the second a whole one.
    expect(stream.state).toBe('connecting')
    await settle(499)
    expect(context.connections).toHaveLength(0)
    await settle(1)
    await settle(999)
    expect(context.connections).toHaveLength(0)
    await settle(1)
    expect(context.connections).toHaveLength(1)
    expect(stream.state).toBe('open')
    stream.close()
  })

  it<Context>('should back off rather than spin when a stream ends having said nothing', async (context) => {
    const stream = open(context)
    await settle()

    context.connections[0].end()
    await settle()
    expect(context.connections).toHaveLength(1)

    await settle(500)
    expect(context.connections).toHaveLength(2)
    stream.close()
  })

  it<Context>('should reconnect after a dropped connection, still resuming', async (context) => {
    const stream = open(context)
    await settle()
    context.connections[0].push(event('e-9', { type: 'live' }))
    await settle()

    context.connections[0].drop()
    await settle(500)

    expect(context.connections).toHaveLength(2)
    expect(context.connections[1].lastEventId).toBe('e-9')
    stream.close()
  })

  it<Context>('should keep delivering after a listener throws, without leaking the connection', async (context) => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
    const heard: string[] = []
    const stream = new EventStream({
      url: 'http://localhost/stream',
      onFrame: (frame) => {
        if (frame.type === 'event' && frame.event === 'poison') {
          throw new Error('this listener cannot handle it')
        }
        heard.push(frame.type)
      },
      fetch: context.fetch,
    })
    await settle()

    context.connections[0].push(event('e-1', { type: 'event', channel: 'c', event: 'poison', payload: 1 }))
    context.connections[0].push(event('e-2', { type: 'event', channel: 'c', event: 'fine', payload: 2 }))
    await settle()

    expect(heard).toEqual([ 'event' ])
    expect(errors).toHaveBeenCalled()
    expect(context.connections).toHaveLength(1)
    expect(context.connections[0].cancelled).toBe(false)
    expect(stream.lastEventId).toBe('e-2')
    stream.close()
  })

  it<Context>('should close the connection and back off when an event overflows the parser', async (context) => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const stream = open(context)
    await settle()

    context.connections[0].push(`data: ${'x'.repeat(200 * 1024)}`)
    await settle()

    expect(context.connections[0].cancelled).toBe(true)
    expect(context.connections).toHaveLength(1)
    await settle(500)
    expect(context.connections).toHaveLength(2)
    stream.close()
  })

  it<Context>('should take a stream that falls silent past two heartbeats for dead', async (context) => {
    const stream = open(context)
    await settle()
    context.connections[0].push(event('e-1', { type: 'live' }))
    await settle()

    // A heartbeat keeps it alive.
    await settle(40_000)
    context.connections[0].push(': heartbeat\n\n')
    await settle(40_000)
    expect(context.connections).toHaveLength(1)

    // Then nothing at all, as a connection that vanished without a FIN.
    await settle(5_000)
    expect(context.connections[0].cancelled).toBe(true)
    await settle(500)
    expect(context.connections).toHaveLength(2)
    expect(context.connections[1].lastEventId).toBe('e-1')
    stream.close()
  })

  it<Context>('should wait and ask again when it is told to, honoring Retry-After', async (context) => {
    context.answers.push(426, { status: 429, retryAfter: '3' })
    const stream = open(context)
    await settle()

    // A rolling deploy's older instance: not final, retried on the backoff.
    expect(stream.state).toBe('connecting')
    await settle(500)
    // Asked to wait three seconds, which outweighs the one-second backoff.
    await settle(2_999)
    expect(context.connections).toHaveLength(0)
    await settle(1)
    expect(context.connections).toHaveLength(1)
    expect(stream.state).toBe('open')
    stream.close()
  })

  it<Context>('should stop for good when the server refuses it', async (context) => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    context.answers.push(401)
    const stream = open(context)
    await settle()

    expect(stream.state).toBe('closed')
    await settle(60_000)
    expect(context.connections).toHaveLength(0)
    expect(warn).toHaveBeenCalled()
  })

  it<Context>('should read its URL anew for every attempt', async (context) => {
    let attempt = 0
    const stream = new EventStream({
      url: () => {
        attempt += 1
        return `http://localhost/stream?attempt=${attempt}`
      },
      onFrame: () => {},
      fetch: context.fetch,
    })
    await settle()
    context.connections[0].push(event('e-1', { type: 'live' }))
    context.connections[0].end()
    await settle(500)

    expect(context.connections.map((connection) => connection.url)).toEqual([
      'http://localhost/stream?attempt=1',
      'http://localhost/stream?attempt=2',
    ])
    stream.close()
  })

  it<Context>('should carry on when its state listener throws', async (context) => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const stream = new EventStream({
      url: 'http://localhost/stream',
      onFrame: (frame) => context.frames.push(frame),
      onStateChange: (state) => {
        if (state === 'open') {
          throw new Error('a bug in the caller')
        }
      },
      fetch: context.fetch,
    })
    await settle()

    context.connections[0].push(event('e-1', { type: 'live' }))
    context.connections[0].end()
    await settle(500)

    expect(context.frames).toEqual([{ type: 'live', id: 'e-1' }])
    expect(context.connections).toHaveLength(2)
    expect(context.connections[1].lastEventId).toBe('e-1')
    expect(error).toHaveBeenCalledWith(
      'An event stream state listener threw',
      expect.objectContaining({ state: 'open' }),
    )
    stream.close()
  })

  it<Context>('should reconnect on its backoff when its URL callback throws', async (context) => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    let attempt = 0
    const stream = new EventStream({
      url: () => {
        attempt += 1
        if (attempt === 1) {
          throw new Error('a bug in the caller')
        }
        return 'http://localhost/stream'
      },
      onFrame: () => {},
      fetch: context.fetch,
    })
    await settle()

    expect(context.connections).toHaveLength(0)
    expect(stream.state).toBe('connecting')
    expect(error).toHaveBeenCalled()

    await settle(500)
    expect(context.connections).toHaveLength(1)
    stream.close()
  })

  it<Context>('should not reconnect once closed', async (context) => {
    const stream = open(context)
    await settle()

    stream.close()
    await settle(60_000)

    expect(stream.state).toBe('closed')
    expect(context.connections).toHaveLength(1)
  })
})
