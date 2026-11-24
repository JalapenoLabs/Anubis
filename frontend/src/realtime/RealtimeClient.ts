// Copyright © 2026 Jalapeno Labs

import type { ConnectionState, EventStreamOptions } from './EventStream'
import type { ListenerRegistry, RealtimeListener } from './listeners'
import type { StreamFrame } from './protocol'

// Core
import { EventStream } from './EventStream'
import { createListenerRegistry } from './listeners'

/** Where the framework serves the realtime stream. */
export const REALTIME_PATH = '/realtime'

export type RealtimeClientOptions = {
  /**
   * Where the stream is served. A path resolves against the page, which is
   * the deployment the framework ships. Defaults to `/realtime`.
   */
  url?: string
  /** The fetch to make requests with. Defaults to the browser's. */
  fetch?: EventStreamOptions['fetch']
  /** Called whenever the connection state changes. */
  onStateChange?: (state: ConnectionState) => void
}

/**
 * One event stream to the Anubis realtime endpoint, shared by every listener.
 *
 * The stream carries every channel that has a listener. It opens with the
 * first one, follows the set as listeners come and go, and closes when the
 * last one leaves. Changes that happen together, such as a page swapping its
 * views, are gathered into one new stream rather than one per change, and the
 * new stream resumes where the old one stopped, so nothing published in
 * between is lost. Callers see none of that: they hold a disposer and hear
 * their channel until they call it.
 *
 * A listener hears every event on its channel, and a `reset` whenever what it
 * holds may be stale: once when its channel goes live, since anything it
 * fetched before then may already be behind, and again whenever a reconnect
 * spanned more than the server keeps. Refetching on a reset is always right.
 *
 * ```ts
 * const client = new RealtimeClient()
 * const stop = client.subscribe(`team:${teamId}:projects`, (message) => {
 *   if (message.type === 'reset') {
 *     void refetchProjects()
 *     return
 *   }
 *   applyToCache(message.event, message.payload)
 * })
 * ```
 */
export class RealtimeClient {
  private readonly options: RealtimeClientOptions
  private readonly listeners: ListenerRegistry = createListenerRegistry()
  private stream: EventStream | null = null
  /** The channels the open stream carries, sorted. */
  private carried: string[] = []
  /**
   * The channels the server has confirmed live on this client's cursor.
   *
   * Only these may resume from it. A channel that joins later was not on the
   * connection the cursor came from, so it opens at the head instead.
   */
  private readonly live = new Set<string>()
  /**
   * The channels the current connection refused, so far.
   *
   * A connection sends its refusals before its `live` frame, and a refused
   * channel must not go live on the cursor: if it is admitted later, it was
   * never heard in between, and only a fresh start gives it the reset it
   * needs.
   */
  private readonly refused = new Set<string>()
  /** The cursor a replacement stream resumes from. */
  private cursor: string | null = null
  /**
   * Which stream is current. A retired stream can still report a state or a
   * frame on its way out, and neither may land on its replacement.
   */
  private generation = 0
  private rebuildTimer: ReturnType<typeof setTimeout> | null = null
  private connectionState: ConnectionState = 'idle'

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
   * Several listeners may share a channel; the stream carries it while any of
   * them is listening.
   */
  subscribe(channel: string, listener: RealtimeListener): () => void {
    this.listeners.add(channel, listener)
    this.scheduleRebuild()

    let released = false
    return () => {
      if (released) {
        console.debug('RealtimeClient disposer called twice', channel)
        return
      }
      released = true

      this.listeners.remove(channel, listener)
      this.scheduleRebuild()
    }
  }

  /**
   * Closes the stream.
   *
   * Listeners are kept, so the next `subscribe` or disposer opens a stream
   * again for everything that is still being listened to, resuming where
   * this one stopped.
   */
  close(): void {
    if (this.rebuildTimer) {
      clearTimeout(this.rebuildTimer)
      this.rebuildTimer = null
    }
    this.retire()
    this.setState('closed')
  }

  /**
   * Rebuilds the stream once the current task is done.
   *
   * One React commit unmounts and mounts many listeners in a row, and they
   * should cost one connection, not one each.
   */
  private scheduleRebuild(): void {
    if (this.rebuildTimer) {
      return
    }

    this.rebuildTimer = setTimeout(() => {
      this.rebuildTimer = null
      this.rebuild()
    }, 0)
  }

  /** Opens a stream for the channels listened to now, unless one already carries them. */
  private rebuild(): void {
    const channels = this.listeners.channels().sort()
    const running = this.stream && this.stream.state !== 'closed'
    const unchanged = channels.length === this.carried.length
      && channels.every((channel, index) => channel === this.carried[index])
    if (running && unchanged) {
      return
    }

    this.retire()
    this.refused.clear()
    // A channel nobody listens to any more stops being live, so one that
    // comes back later joins at the head rather than on an old cursor.
    for (const channel of [ ...this.live ]) {
      if (!channels.includes(channel)) {
        this.live.delete(channel)
      }
    }
    this.carried = channels

    if (!channels.length) {
      this.setState('idle')
      return
    }

    const generation = this.generation
    this.stream = new EventStream({
      url: () => this.streamUrl(),
      lastEventId: this.cursor,
      onFrame: (frame) => {
        if (generation === this.generation) {
          this.receive(frame)
        }
      },
      onStateChange: (state) => {
        if (generation === this.generation) {
          this.setState(state)
        }
      },
      fetch: this.options.fetch,
    })
  }

  /** Closes the current stream, keeping its cursor for the next one. */
  private retire(): void {
    this.generation += 1
    if (!this.stream) {
      return
    }
    this.cursor = this.stream.lastEventId ?? this.cursor
    this.stream.close()
    this.stream = null
  }

  /**
   * The URL for the next connection: every carried channel, and which of
   * them are fresh, because they have not gone live on the cursor yet.
   *
   * Read before every attempt, so a reconnection after a drop resumes every
   * channel the stream confirmed live before it dropped. With no cursor at
   * all, every channel is fresh and the server needs telling of none.
   */
  private streamUrl(): string {
    const parameters = new URLSearchParams({ channels: this.carried.join(',') })
    const cursor = this.stream?.lastEventId ?? this.cursor
    const fresh = this.carried.filter((channel) => !this.live.has(channel))
    if (cursor && fresh.length) {
      parameters.set('fresh', fresh.join(','))
    }

    return `${this.options.url ?? REALTIME_PATH}?${parameters.toString()}`
  }

  /** Routes one frame: messages to their listeners, the rest to the client. */
  private receive(frame: StreamFrame): void {
    if (frame.type === 'event' || frame.type === 'reset') {
      this.listeners.emit(frame)
      return
    }

    if (frame.type === 'live') {
      // Everything the stream was opened for and not refused is now on this
      // cursor.
      for (const channel of this.carried) {
        if (!this.refused.has(channel)) {
          this.live.add(channel)
        }
      }
      this.refused.clear()
      return
    }

    // Refused, so not live: should it be admitted on a later connection, it
    // opens fresh, with the reset that covers whatever it missed meanwhile.
    this.refused.add(frame.channel)
    this.live.delete(frame.channel)
    console.warn('The realtime server refused a channel', frame)
  }

  private setState(state: ConnectionState): void {
    if (this.connectionState === state) {
      return
    }

    this.connectionState = state
    this.options.onStateChange?.(state)
  }
}
