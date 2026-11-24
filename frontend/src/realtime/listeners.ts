// Copyright © 2026 Jalapeno Labs

import type { RealtimeMessage } from './protocol'

/**
 * Called with everything a subscribed channel hears: each event, and each
 * reset telling the listener to read the channel's state again.
 *
 * A listener that only refetches can treat both alike. One that writes an
 * event's payload into a cache has to branch on `message.type`, and the
 * compiler holds it to that: a reset carries no payload.
 */
export type RealtimeListener = (message: RealtimeMessage) => void

/**
 * Who is listening to what, for one client.
 *
 * Several components may watch the same channel, so listeners are grouped by
 * channel name and a channel exists exactly as long as somebody is listening
 * to it. `channels()` is therefore both the list a stream is opened for and
 * the answer to whether the last listener just left.
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
   * Delivers one message to its channel's listeners.
   *
   * Listeners are copied before the walk so that one unsubscribing inside its
   * own callback cannot skip the next one, and a listener that throws does not
   * rob the others of the message.
   */
  function emit(message: RealtimeMessage): void {
    const listeners = listenersByChannel.get(message.channel)
    if (!listeners?.size) {
      console.debug('createListenerRegistry received a message nobody is listening to', message)
      return
    }

    for (const listener of [ ...listeners ]) {
      try {
        listener(message)
      }
      catch (error) {
        console.error('A realtime listener threw', { message, error })
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
