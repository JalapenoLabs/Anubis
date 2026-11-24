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
 * The longest a server's `Retry-After` is honored for, in milliseconds.
 *
 * A server asking for a minute is asking for patience; one asking for an hour
 * is mistaken, and a tab that waits an hour looks broken.
 */
const RETRY_AFTER_CEILING = 60_000

/**
 * How long a stream may say nothing at all before it is taken for dead, in
 * milliseconds.
 *
 * The server sends a heartbeat comment whenever it has been quiet for twenty
 * seconds, so silence past two of them means the connection is gone without
 * having closed: a server killed without a FIN, or a NAT that forgot it. The
 * stream then reconnects rather than stay open and deaf.
 */
const IDLE_LIMIT = 45_000

/**
 * The most text one event may buffer before the stream is abandoned.
 *
 * The server never renders an event past 64 KiB, so twice that is room to
 * spare for any honest stream and a ceiling on what a broken one can make
 * the parser hold.
 */
const MAX_EVENT_CHARACTERS = 128 * 1024

/**
 * The refusals worth asking again after a wait.
 *
 * `408` and `429` say "not now"; `425` says "too early"; `426` is what an
 * instance still running a release that predates event streams answers during
 * a rolling deploy. Every other `4xx` will be answered the same way next time,
 * so it is final.
 */
const RETRYABLE_REFUSALS: ReadonlySet<number> = new Set([ 408, 425, 426, 429 ])

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

/** How one connection ended, and how long the server asked to be left. */
type Ending = {
  kind:
    /** The server closed it after sending something: reconnect at once and resume. */
    | 'ended'
    /** It failed, broke, or ended having said nothing: reconnect on the backoff. */
    | 'failed'
    /** The server refused it, and will refuse it again: stop. */
    | 'refused'
  /** The least the next attempt waits, in milliseconds, from `Retry-After`. */
  waitAtLeast: number
}

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
 * at once, and one that fails, or falls silent past two heartbeats, reconnects
 * on a capped backoff. Every reconnection sends the id of the last event or
 * `live` frame it heard as `Last-Event-ID`, so the server replays what was
 * missed, or resets the channels it can no longer replay. A refusal that will
 * not change, such as `401`, is final: the stream closes rather than ask again
 * for what it was refused. One that may (`408`, `425`, `426`, `429`) waits and
 * asks again, honoring `Retry-After`.
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

      if (ending.kind === 'refused') {
        this.setState('closed')
        return
      }

      const backoff = ending.kind === 'ended'
        ? 0
        : reconnectDelay(this.attempt)
      if (ending.kind === 'failed') {
        this.attempt += 1
      }
      this.setState('connecting')
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null
        this.connect()
      }, Math.max(backoff, ending.waitAtLeast))
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
      return { kind: 'failed', waitAtLeast: 0 }
    }

    const waitAtLeast = retryAfter(response)
    if (response.status >= 400 && response.status < 500) {
      if (RETRYABLE_REFUSALS.has(response.status)) {
        console.debug('The event stream was asked to wait; it will retry', response.status)
        return { kind: 'failed', waitAtLeast }
      }
      console.warn('The server refused the event stream, so it will not reconnect', response.status)
      return { kind: 'refused', waitAtLeast }
    }
    if (!response.ok || !response.body) {
      console.debug('The event stream could not open; it will retry', response.status)
      return { kind: 'failed', waitAtLeast }
    }
    if (!response.headers.get('content-type')?.startsWith('text/event-stream')) {
      console.warn('The server answered the event stream with something else, so it will not reconnect')
      await response.body.cancel().catch((error: unknown) => {
        console.debug('EventStream could not cancel an answer it will not read', error)
      })
      return { kind: 'refused', waitAtLeast }
    }

    this.attempt = 0
    this.setState('open')
    return this.consume(response.body, signal)
  }

  /**
   * Reads an open stream's body until it ends, breaks, or falls silent.
   *
   * Whatever ends it, the body is cancelled on the way out, so a stream this
   * client stopped reading never stays open on the server.
   */
  private async consume(body: ReadableStream<Uint8Array>, signal: AbortSignal): Promise<Ending> {
    const reader = body.getReader()
    const decoder = new TextDecoder()

    // A server that ends a stream before sending a thing is one to back off
    // from, not one to reconnect to in a tight loop.
    let heardAnything = false
    let overflowed = false
    let stalled = false
    let watchdog: ReturnType<typeof setTimeout> | null = null
    const restartWatchdog = () => {
      if (watchdog) {
        clearTimeout(watchdog)
      }
      watchdog = setTimeout(() => {
        stalled = true
        void reader.cancel().catch((error: unknown) => {
          console.debug('EventStream could not cancel a silent stream', error)
        })
      }, IDLE_LIMIT)
    }

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
        if (!frame) {
          return
        }
        // A listener that throws is the listener's problem: the event was
        // delivered, and the stream carries on to the next one.
        try {
          this.options.onFrame(frame)
        }
        catch (error) {
          console.error('An event stream listener threw', { frame, error })
        }
      },
      onError: (error) => {
        if (error.type === 'max-buffer-size-exceeded') {
          overflowed = true
        }
        console.warn('The event stream sent something that is not an event', error)
      },
    })

    restartWatchdog()
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (stalled) {
          console.debug('The event stream fell silent past its heartbeats; it will reconnect')
          return { kind: 'failed', waitAtLeast: 0 }
        }
        if (done) {
          parser.feed(decoder.decode())
          return {
            kind: heardAnything
              ? 'ended'
              : 'failed',
            waitAtLeast: 0,
          }
        }

        restartWatchdog()
        parser.feed(decoder.decode(value, { stream: true }))
        if (overflowed) {
          return { kind: 'failed', waitAtLeast: 0 }
        }
      }
    }
    catch (error) {
      if (!signal.aborted) {
        console.debug('The event stream broke; it will reconnect', error)
      }
      return { kind: 'failed', waitAtLeast: 0 }
    }
    finally {
      if (watchdog) {
        clearTimeout(watchdog)
      }
      // Cancelling a body that already ended or broke does nothing; one that
      // is still open is closed here rather than left to the server.
      void reader.cancel().catch((error: unknown) => {
        console.debug('EventStream could not cancel a stream it stopped reading', error)
      })
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

/** The wait a response's `Retry-After` asks for, in milliseconds, or none. */
function retryAfter(response: Response): number {
  const seconds = Number(response.headers.get('retry-after'))
  if (!Number.isFinite(seconds) || seconds <= 0) {
    return 0
  }
  return Math.min(seconds * 1000, RETRY_AFTER_CEILING)
}
