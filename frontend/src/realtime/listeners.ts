// Copyright © 2026 Jalapeno Labs

import type { RealtimeEvent } from './protocol'

/** Called with every event published on a subscribed channel. */
export type RealtimeListener = (event: RealtimeEvent) => void

/**
 * Who is listening to what, for one connection.
 *
 * Several components may watch the same channel, so listeners are grouped by
 * channel name and a channel exists exactly as long as somebody is listening
 * to it. `channels()` is therefore both the resubscribe list after a reconnect
 * and the answer to whether the last listener just left.
 */
export function createListenerRegistry() {
  const listenersByChannel = new Map<string, Set<RealtimeListener>>()

  /** Adds a listener, returning whether the channel is newly interesting. */
  function add(channel: string, listener: RealtimeListener): boolean {
    const listeners = listenersByChannel.get(channel)
    if (listeners) {
      listeners.add(listener)
      return false
    }

    listenersByChannel.set(channel, new Set([ listener ]))
    return true
  }

  /** Removes a listener, returning whether the channel is now unwatched. */
  function remove(channel: string, listener: RealtimeListener): boolean {
    const listeners = listenersByChannel.get(channel)
    if (!listeners?.delete(listener)) {
      console.debug('createListenerRegistry removed a listener it did not hold', channel)
      return false
    }

    if (listeners.size) {
      return false
    }

    listenersByChannel.delete(channel)
    return true
  }

  /**
   * Delivers one event to the channel's listeners.
   *
   * Listeners are copied before the walk so that one unsubscribing inside its
   * own callback cannot skip the next one, and a listener that throws does not
   * rob the others of the event.
   */
  function emit(event: RealtimeEvent): void {
    const listeners = listenersByChannel.get(event.channel)
    if (!listeners?.size) {
      console.debug('createListenerRegistry received an event nobody is listening to', event)
      return
    }

    for (const listener of [ ...listeners ]) {
      try {
        listener(event)
      }
      catch (error) {
        console.error('A realtime listener threw', { event, error })
      }
    }
  }

  return {
    add,
    remove,
    emit,
    /** Every channel with at least one listener. */
    channels: () => [ ...listenersByChannel.keys() ],
    /** Whether anybody is listening to a channel. */
    has: (channel: string) => listenersByChannel.has(channel),
  } as const
}

export type ListenerRegistry = ReturnType<typeof createListenerRegistry>
