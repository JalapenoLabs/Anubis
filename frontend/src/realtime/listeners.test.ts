// Copyright © 2026 Jalapeno Labs

import type { RealtimeEvent } from './protocol'

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { createListenerRegistry } from './listeners'

const CHANNEL = 'team:11111111-1111-1111-1111-111111111111:projects'
const OTHER_CHANNEL = 'user:22222222-2222-2222-2222-222222222222:inbox'

function eventOn(channel: string): RealtimeEvent {
  return { channel, event: 'created', payload: { id: 7 }}
}

describe('createListenerRegistry', () => {
  it('should report only the first listener of a channel as new', () => {
    const registry = createListenerRegistry()

    expect(registry.add(CHANNEL, () => {})).toBe(true)
    expect(registry.add(CHANNEL, () => {})).toBe(false)
    expect(registry.add(OTHER_CHANNEL, () => {})).toBe(true)
  })

  it('should report only the last listener of a channel as leaving', () => {
    const registry = createListenerRegistry()
    const first = () => {}
    const second = () => {}
    registry.add(CHANNEL, first)
    registry.add(CHANNEL, second)

    expect(registry.remove(CHANNEL, first)).toBe(false)
    expect(registry.has(CHANNEL)).toBe(true)

    expect(registry.remove(CHANNEL, second)).toBe(true)
    expect(registry.has(CHANNEL)).toBe(false)
  })

  it('should shrug off removing a listener it never held', () => {
    const registry = createListenerRegistry()
    registry.add(CHANNEL, () => {})

    expect(registry.remove(CHANNEL, () => {})).toBe(false)
    expect(registry.remove(OTHER_CHANNEL, () => {})).toBe(false)
    expect(registry.has(CHANNEL)).toBe(true)
  })

  it('should list every channel with a listener, for resubscribing', () => {
    const registry = createListenerRegistry()
    const leaving = () => {}
    registry.add(CHANNEL, () => {})
    registry.add(OTHER_CHANNEL, leaving)

    expect(registry.channels()).toEqual([ CHANNEL, OTHER_CHANNEL ])

    registry.remove(OTHER_CHANNEL, leaving)
    expect(registry.channels()).toEqual([ CHANNEL ])
  })

  it('should deliver an event to every listener of its channel and no other', () => {
    const registry = createListenerRegistry()
    const received: string[] = []
    registry.add(CHANNEL, () => received.push('first'))
    registry.add(CHANNEL, () => received.push('second'))
    registry.add(OTHER_CHANNEL, () => received.push('elsewhere'))

    registry.emit(eventOn(CHANNEL))

    expect(received).toEqual([ 'first', 'second' ])
  })

  it('should still reach the other listeners when one throws', () => {
    const registry = createListenerRegistry()
    const received: string[] = []
    registry.add(CHANNEL, () => {
      throw new Error('this listener is broken')
    })
    registry.add(CHANNEL, () => received.push('reached'))

    registry.emit(eventOn(CHANNEL))

    expect(received).toEqual([ 'reached' ])
  })

  it('should let a listener unsubscribe from inside its own callback', () => {
    const registry = createListenerRegistry()
    const received: string[] = []
    const once = () => {
      received.push('once')
      registry.remove(CHANNEL, once)
    }
    registry.add(CHANNEL, once)
    registry.add(CHANNEL, () => received.push('after'))

    registry.emit(eventOn(CHANNEL))
    registry.emit(eventOn(CHANNEL))

    expect(received).toEqual([ 'once', 'after', 'after' ])
  })

  it('should drop an event nobody is listening to', () => {
    const registry = createListenerRegistry()

    // Nothing to assert but that this neither throws nor reaches anyone.
    registry.emit(eventOn(CHANNEL))

    expect(registry.channels()).toEqual([])
  })
})
