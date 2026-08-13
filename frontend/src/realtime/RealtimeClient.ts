// Copyright © 2026 Jalapeno Labs

import type { ListenerRegistry, RealtimeListener } from './listeners'
import type { ClientFrame } from './protocol'

// Core
import { createListenerRegistry } from './listeners'

// Misc
import { parseServerFrame } from './protocol'

/** Where the framework mounts the realtime socket. */
export const REALTIME_PATH = '/realtime'

/** How long the first reconnection waits, in milliseconds. */
const RECONNECT_BASE = 500

/** The longest a reconnection ever waits, in milliseconds. */
const RECONNECT_CEILING = 10_000

/** What the client is doing about its connection. */
export type ConnectionState = 'idle' | 'connecting' | 'open' | 'closed'

/**
 * The part of the browser's WebSocket this client uses.
 *
 * Narrow on purpose: a real `WebSocket` satisfies it, and so does a fake in a
 * test, which is what makes the reconnect and resubscribe behavior testable
 * without a server.
 */
export type WebSocketLike = {
  send: (data: string) => void
  close: () => void
  onopen: ((event: Event) => void) | null
  onclose: ((event: CloseEvent) => void) | null
  onerror: ((event: Event) => void) | null
  onmessage: ((event: MessageEvent) => void) | null
}

export type RealtimeClientOptions = {
  /**
   * Where to connect.
   *
   * A `ws://` or `wss://` URL is used as given. Anything else is treated as a
   * path on the current origin, which is the deployment the framework ships:
   * one binary serving both the API and the socket. Defaults to `/realtime`.
   */
  url?: string
  /** Builds the socket. Defaults to the browser's `WebSocket`. */
  createSocket?: (url: string) => WebSocketLike
  /** Called whenever the connection state changes. */
  onStateChange?: (state: ConnectionState) => void
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
 * One websocket to the Anubis realtime endpoint, shared by every subscriber.
 *
 * Connects on the first subscription and stays connected: dropped connections
 * are retried on a capped exponential backoff, and every channel that still
 * has a listener is resubscribed as soon as the new connection opens. Callers
 * see none of that; they hold a disposer and get events until they call it.
 *
 * Realtime is a notification bus, not a log. Events published while the socket
 * was down are not replayed, so a listener that must not miss state should
 * refetch when it reconnects.
 *
 * ```ts
 * const client = new RealtimeClient()
 * const stop = client.subscribe(`team:${teamId}:projects`, (event) => {
 *   console.log(event.event, event.payload)
 * })
 * ```
 */
export class RealtimeClient {
  private readonly options: RealtimeClientOptions
  private readonly listeners: ListenerRegistry = createListenerRegistry()
  private socket: WebSocketLike | null = null
  private connectionState: ConnectionState = 'idle'
  private attempt = 0
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null

  constructor(options: RealtimeClientOptions = {}) {
    this.options = options
  }

  /** What the client is currently doing about its connection. */
  get state(): ConnectionState {
    return this.connectionState
  }

  /**
   * Listens to `channel` until the returned disposer is called.
   *
   * Subscribing connects if the client is not connected yet. Several
   * listeners may share a channel; the server is told only when the first
   * arrives and when the last leaves.
   */
  subscribe(channel: string, listener: RealtimeListener): () => void {
    const isFirstListener = this.listeners.add(channel, listener)
    this.connect()

    if (isFirstListener) {
      this.send({ type: 'subscribe', channel })
    }

    let released = false
    return () => {
      if (released) {
        console.debug('RealtimeClient disposer called twice', channel)
        return
      }
      released = true

      if (this.listeners.remove(channel, listener)) {
        this.send({ type: 'unsubscribe', channel })
      }
    }
  }

  /**
   * Closes the connection and stops reconnecting.
   *
   * Listeners are kept, so a later `subscribe` reopens the connection with
   * everything that is still being listened to.
   */
  close(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }

    this.setState('closed')
    this.socket?.close()
    this.socket = null
  }

  /** Opens a connection unless one is already open or on its way. */
  private connect(): void {
    if (this.socket || this.reconnectTimer) {
      return
    }

    const url = this.options.url
    const createSocket = this.options.createSocket ?? ((target: string) => new WebSocket(target))

    this.setState('connecting')
    const socket = createSocket(resolveUrl(url))
    this.socket = socket

    socket.onopen = () => {
      this.attempt = 0
      this.setState('open')
      // A new connection knows nothing about the old one's subscriptions.
      for (const channel of this.listeners.channels()) {
        this.send({ type: 'subscribe', channel })
      }
    }

    socket.onmessage = (event: MessageEvent) => {
      const frame = parseServerFrame(event.data)
      if (!frame) {
        return
      }

      if (frame.type === 'event') {
        this.listeners.emit({
          channel: frame.channel,
          event: frame.event,
          payload: frame.payload,
        })
        return
      }

      if (frame.type === 'error') {
        console.warn('The realtime server refused a request', frame)
      }
    }

    socket.onerror = (event: Event) => {
      // The close that follows is what actually drives the reconnect; this is
      // only worth saying out loud so a failure is visible in the console.
      console.debug('The realtime socket reported an error', event)
    }

    socket.onclose = () => {
      this.socket = null
      if (this.connectionState === 'closed') {
        return
      }
      this.scheduleReconnect()
    }
  }

  /** Waits out the backoff, then connects again. */
  private scheduleReconnect(): void {
    const delay = reconnectDelay(this.attempt)
    this.attempt += 1
    this.setState('connecting')

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.connect()
    }, delay)
  }

  /** Sends one frame, if there is an open connection to send it on. */
  private send(frame: ClientFrame): void {
    if (!this.socket || this.connectionState !== 'open') {
      // Not a failure: everything still listened to is resubscribed the
      // moment the connection opens.
      console.debug('RealtimeClient deferred a frame until the socket opens', frame)
      return
    }

    this.socket.send(JSON.stringify(frame))
  }

  private setState(state: ConnectionState): void {
    if (this.connectionState === state) {
      return
    }

    this.connectionState = state
    this.options.onStateChange?.(state)
  }
}

/**
 * Turns the configured URL into the absolute one the socket connects to.
 *
 * A path is resolved against the page, upgrading `https` to `wss`, which is
 * what the one-binary deployment wants and what a Vite dev server proxies.
 */
function resolveUrl(url: string | undefined): string {
  if (url?.startsWith('ws://') || url?.startsWith('wss://')) {
    return url
  }

  const path = url || REALTIME_PATH
  if (typeof window === 'undefined') {
    throw new Error(
      'RealtimeClient needs an absolute ws:// or wss:// url when there is no page to resolve against',
    )
  }

  const scheme = window.location.protocol === 'https:'
    ? 'wss:'
    : 'ws:'
  return `${scheme}//${window.location.host}${path}`
}
