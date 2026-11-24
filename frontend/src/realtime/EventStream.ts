// Copyright © 2026 Jalapeno Labs

import type { StreamFrame } from './protocol'
import type { Options } from 'ky'

// Core
import ky from 'ky'
import { createParser } from 'eventsource-parser'

// Misc
import { parseStreamFrame } from './protocol'

/** How long the first reconnection after a failure waits, in milliseconds. */
const RECONNECT_BASE = 500

/** The longest a reconnection ever waits, in milliseconds. */
const RECONNECT_CEILING = 10_000

/**
 * The most text one event may buffer before the stream is abandoned.
 *
 * The server never renders an event past 64 KiB, so twice that is room to
 * spare for any honest stream and a ceiling on what a broken one can make
 * the parser hold.
 */
const MAX_EVENT_CHARACTERS = 128 * 1024

/** What a stream is doing about its connection. */
export type ConnectionState = 'idle' | 'connecting' | 'open' | 'closed'

export type EventStreamOptions = {
  /**
   * Where to connect, read again before every attempt, so a client can
   * change what it asks for between reconnects. A path resolves against the
   * page, which is the deployment the framework ships: one origin serving
   * both the API and its streams.
   */
  url: string | (() => string)
  /** The id to resume after on the first attempt, sent as `Last-Event-ID`. */
  lastEventId?: string | null
  /** Called with every frame the stream carries. */
  onFrame: (frame: StreamFrame) => void
  /** Called whenever the connection state changes. */
  onStateChange?: (state: ConnectionState) => void
  /** The fetch to make requests with. Defaults to the browser's. */
  fetch?: Options['fetch']
}

/** How one connection ended. */
type Ending =
  /** The server closed it after sending something: reconnect at once and resume. */
  | 'ended'
  /** It failed, broke, or ended having said nothing: reconnect on the backoff. */
  | 'failed'
  /** The server refused it: it will refuse it again, so stop. */
  | 'refused'

/**
 * How long to wait before the reconnection attempt numbered `attempt`.
 *
 * Doubling from half a second and capped at ten seconds: a server that
 * restarts is picked up almost at once, while a server that is genuinely down
 * is not hammered by every open tab. The schedule is deliberately free of
 * jitter, because a browser tab is one client, not a fleet, and a predictable
 * schedule is one a person can recognize in a network panel.
 */
export function reconnectDelay(attempt: number): number {
  const delay = RECONNECT_BASE * 2 ** Math.max(attempt, 0)
  return Math.min(delay, RECONNECT_CEILING)
}

/**
 * One Server-Sent Events stream that resumes wherever it left off.
 *
 * Connects as soon as it is built and stays connected: a stream the server
 * ends, which it does every few minutes and whenever it restarts, reconnects
 * at once, and one that fails reconnects on a capped backoff. Every
 * reconnection sends the id of the last event or `live` frame it heard as
 * `Last-Event-ID`, so the server replays what was missed, or resets the
 * channels it can no longer replay. A `4xx` answer is final: the stream
 * closes rather than ask again for what it was refused.
 *
 * Built on `fetch` rather than the browser's `EventSource`, which can send
 * `Last-Event-ID` only on its own reconnects and never says why a connection
 * failed, so a stream that changes what it asks for could not resume and a
 * signed-out one could never stop.
 *
 * ```ts
 * const stream = new EventStream({
 *   url: `/api/v1/servers/${serverId}/logs`,
 *   onFrame: (frame) => {
 *     if (frame.type === 'event') {
 *       appendLine(frame.payload)
 *     }
 *   },
 * })
 * ```
 */
export class EventStream {
  private readonly options: EventStreamOptions
  private connectionState: ConnectionState = 'idle'
  private cursor: string | null
  private attempt = 0
  private controller: AbortController | null = null
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null

  constructor(options: EventStreamOptions) {
    this.options = options
    this.cursor = options.lastEventId ?? null
    this.connect()
  }

  /** What the stream is currently doing about its connection. */
  get state(): ConnectionState {
    return this.connectionState
  }

  /** The id the next connection resumes after, when the stream has one. */
  get lastEventId(): string | null {
    return this.cursor
  }

  /** Ends the stream for good: the connection closes and nothing reconnects. */
  close(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.controller?.abort()
    this.controller = null
    this.setState('closed')
  }

  /** Opens one connection, and decides what happens when it ends. */
  private connect(): void {
    const controller = new AbortController()
    this.controller = controller
    this.setState('connecting')

    void this.read(controller.signal).then((ending) => {
      if (controller.signal.aborted) {
        // Closed on purpose; the caller has moved on.
        return
      }
      this.controller = null

      if (ending === 'refused') {
        this.setState('closed')
        return
      }

      const delay = ending === 'ended'
        ? 0
        : reconnectDelay(this.attempt)
      if (ending === 'failed') {
        this.attempt += 1
      }
      this.setState('connecting')
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null
        this.connect()
      }, delay)
    })
  }

  /** Reads one connection from its request to its end. */
  private async read(signal: AbortSignal): Promise<Ending> {
    const headers: Record<string, string> = { Accept: 'text/event-stream' }
    if (this.cursor) {
      headers['Last-Event-ID'] = this.cursor
    }

    const url = typeof this.options.url === 'function'
      ? this.options.url()
      : this.options.url

    let response: Response
    try {
      response = await ky.get(url, {
        headers,
        signal,
        // A stream is meant to stay open, and reconnecting is this class's
        // job, so ky's own timeout, retries, and error throwing are all off.
        timeout: false,
        retry: 0,
        throwHttpErrors: false,
        cache: 'no-store',
        credentials: 'same-origin',
        fetch: this.options.fetch,
      })
    }
    catch (error) {
      if (!signal.aborted) {
        console.debug('EventStream could not connect; it will retry', error)
      }
      return 'failed'
    }

    if (response.status >= 400 && response.status < 500) {
      console.warn('The server refused the event stream, so it will not reconnect', response.status)
      return 'refused'
    }
    if (!response.ok || !response.body) {
      console.debug('The event stream could not open; it will retry', response.status)
      return 'failed'
    }
    if (!response.headers.get('content-type')?.startsWith('text/event-stream')) {
      console.warn('The server answered the event stream with something else, so it will not reconnect')
      return 'refused'
    }

    this.attempt = 0
    this.setState('open')

    // A server that ends a stream before sending a thing is one to back off
    // from, not one to reconnect to in a tight loop.
    let heardAnything = false
    const parser = createParser({
      maxBufferSize: MAX_EVENT_CHARACTERS,
      onEvent: (message) => {
        heardAnything = true
        // Per the standard, an id moves the cursor even on a frame this
        // client cannot read.
        if (message.id) {
          this.cursor = message.id
        }
        const frame = parseStreamFrame(message.data, message.id)
        if (frame) {
          this.options.onFrame(frame)
        }
      },
      onError: (error) => {
        console.warn('The event stream sent something that is not an event', error)
      },
    })

    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) {
          parser.feed(decoder.decode())
          return heardAnything
            ? 'ended'
            : 'failed'
        }
        parser.feed(decoder.decode(value, { stream: true }))
      }
    }
    catch (error) {
      if (!signal.aborted) {
        console.debug('The event stream broke; it will reconnect', error)
      }
      return 'failed'
    }
  }

  private setState(state: ConnectionState): void {
    if (this.connectionState === state) {
      return
    }

    this.connectionState = state
    this.options.onStateChange?.(state)
  }
}
