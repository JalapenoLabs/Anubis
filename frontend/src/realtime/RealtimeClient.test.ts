// Copyright © 2026 Jalapeno Labs

import type { RealtimeEvent } from './protocol'
import type { WebSocketLike } from './RealtimeClient'

// Core
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Misc
import { RealtimeClient, reconnectDelay } from './RealtimeClient'

const CHANNEL = 'team:11111111-1111-1111-1111-111111111111:projects'
const OTHER_CHANNEL = 'user:22222222-2222-2222-2222-222222222222:inbox'

type FakeSocket = WebSocketLike & {
  sent: string[]
  isClosed: boolean
  /** The server accepted the connection. */
  accept: () => void
  /** The connection dropped without the client asking. */
  drop: () => void
  /** The server sent a frame. */
  deliver: (frame: unknown) => void
}

function createFakeSocket(): FakeSocket {
  const socket: FakeSocket = {
    sent: [],
    isClosed: false,
    onopen: null,
    onclose: null,
    onerror: null,
    onmessage: null,
    send: (data: string) => {
      socket.sent.push(data)
    },
    close: () => {
      socket.isClosed = true
      socket.onclose?.(new CloseEvent('close'))
    },
    accept: () => socket.onopen?.(new Event('open')),
    drop: () => socket.onclose?.(new CloseEvent('close')),
    deliver: (frame: unknown) => {
      socket.onmessage?.(new MessageEvent('message', { data: JSON.stringify(frame) }))
    },
  }

  return socket
}

/** Every socket the client under test opened, oldest first. */
type Context = {
  sockets: FakeSocket[]
  client: RealtimeClient
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

describe('RealtimeClient', () => {
  beforeEach<Context>((context) => {
    vi.useFakeTimers()

    context.sockets = []
    context.client = new RealtimeClient({
      url: 'ws://localhost/realtime',
      createSocket: () => {
        const socket = createFakeSocket()
        context.sockets.push(socket)
        return socket
      },
    })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it<Context>('should connect on the first subscription and subscribe once open', (context) => {
    context.client.subscribe(CHANNEL, () => {})

    expect(context.sockets).toHaveLength(1)
    expect(context.client.state).toBe('connecting')
    // Nothing can be sent before the handshake finishes.
    expect(context.sockets[0].sent).toEqual([])

    context.sockets[0].accept()

    expect(context.client.state).toBe('open')
    expect(context.sockets[0].sent).toEqual([
      JSON.stringify({ type: 'subscribe', channel: CHANNEL }),
    ])
  })

  it<Context>('should tell the server about a channel once, however many listeners it has', (context) => {
    const stopFirst = context.client.subscribe(CHANNEL, () => {})
    context.sockets[0].accept()
    const stopSecond = context.client.subscribe(CHANNEL, () => {})

    expect(context.sockets[0].sent).toEqual([
      JSON.stringify({ type: 'subscribe', channel: CHANNEL }),
    ])

    stopFirst()
    expect(context.sockets[0].sent).toHaveLength(1)

    stopSecond()
    expect(context.sockets[0].sent).toEqual([
      JSON.stringify({ type: 'subscribe', channel: CHANNEL }),
      JSON.stringify({ type: 'unsubscribe', channel: CHANNEL }),
    ])
  })

  it<Context>('should deliver events to the listeners of their own channel only', (context) => {
    const received: RealtimeEvent[] = []
    context.client.subscribe(CHANNEL, (event) => received.push(event))
    context.client.subscribe(OTHER_CHANNEL, () => {
      throw new Error('the other channel must not hear this')
    })
    context.sockets[0].accept()

    context.sockets[0].deliver({
      type: 'event',
      channel: CHANNEL,
      event: 'created',
      payload: { id: 7 },
    })

    expect(received).toEqual([
      { channel: CHANNEL, event: 'created', payload: { id: 7 }},
    ])
  })

  it<Context>('should ignore a frame it cannot understand', (context) => {
    const received: RealtimeEvent[] = []
    context.client.subscribe(CHANNEL, (event) => received.push(event))
    context.sockets[0].accept()

    context.sockets[0].onmessage?.(new MessageEvent('message', { data: 'not json' }))
    context.sockets[0].deliver({ type: 'from-the-future', channel: CHANNEL })

    expect(received).toEqual([])
    expect(context.client.state).toBe('open')
  })

  it<Context>('should reconnect after a drop and resubscribe everything', (context) => {
    context.client.subscribe(CHANNEL, () => {})
    context.client.subscribe(OTHER_CHANNEL, () => {})
    context.sockets[0].accept()

    context.sockets[0].drop()
    expect(context.client.state).toBe('connecting')
    expect(context.sockets).toHaveLength(1)

    // Nothing happens before the first delay is spent.
    vi.advanceTimersByTime(499)
    expect(context.sockets).toHaveLength(1)

    vi.advanceTimersByTime(1)
    expect(context.sockets).toHaveLength(2)

    context.sockets[1].accept()
    expect(context.sockets[1].sent).toEqual([
      JSON.stringify({ type: 'subscribe', channel: CHANNEL }),
      JSON.stringify({ type: 'subscribe', channel: OTHER_CHANNEL }),
    ])
  })

  it<Context>('should double the wait between attempts that never connect', (context) => {
    context.client.subscribe(CHANNEL, () => {})
    context.sockets[0].accept()

    // The first attempt after a working connection waits the base delay.
    context.sockets[0].drop()
    vi.advanceTimersByTime(500)
    expect(context.sockets).toHaveLength(2)

    // This attempt never opened, so the next one waits twice as long.
    context.sockets[1].drop()
    vi.advanceTimersByTime(999)
    expect(context.sockets).toHaveLength(2)
    vi.advanceTimersByTime(1)
    expect(context.sockets).toHaveLength(3)

    // And the one after that waits twice as long again.
    context.sockets[2].drop()
    vi.advanceTimersByTime(1999)
    expect(context.sockets).toHaveLength(3)
    vi.advanceTimersByTime(1)
    expect(context.sockets).toHaveLength(4)
  })

  it<Context>('should restart the backoff after a connection succeeds', (context) => {
    context.client.subscribe(CHANNEL, () => {})
    context.sockets[0].accept()

    context.sockets[0].drop()
    vi.advanceTimersByTime(500)
    context.sockets[1].accept()

    context.sockets[1].drop()
    vi.advanceTimersByTime(500)

    expect(context.sockets).toHaveLength(3)
  })

  it<Context>('should stop reconnecting once it is closed', (context) => {
    context.client.subscribe(CHANNEL, () => {})
    context.sockets[0].accept()

    context.client.close()

    expect(context.client.state).toBe('closed')
    expect(context.sockets[0].isClosed).toBe(true)

    vi.advanceTimersByTime(60_000)
    expect(context.sockets).toHaveLength(1)
  })

  it<Context>('should reopen with everything still listened to after a close', (context) => {
    context.client.subscribe(CHANNEL, () => {})
    context.sockets[0].accept()
    context.client.close()

    context.client.subscribe(OTHER_CHANNEL, () => {})
    expect(context.sockets).toHaveLength(2)

    context.sockets[1].accept()
    expect(context.sockets[1].sent).toEqual([
      JSON.stringify({ type: 'subscribe', channel: CHANNEL }),
      JSON.stringify({ type: 'subscribe', channel: OTHER_CHANNEL }),
    ])
  })
})
