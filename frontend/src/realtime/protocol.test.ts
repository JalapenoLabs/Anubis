// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { parseServerFrame } from './protocol'

const CHANNEL = 'team:11111111-1111-1111-1111-111111111111:projects'

describe('parseServerFrame', () => {
  it('should read an event frame', () => {
    const frame = parseServerFrame(JSON.stringify({
      type: 'event',
      channel: CHANNEL,
      event: 'created',
      payload: { id: 7, name: 'Apollo' },
    }))

    expect(frame).toEqual({
      type: 'event',
      channel: CHANNEL,
      event: 'created',
      payload: { id: 7, name: 'Apollo' },
    })
  })

  it('should read an event whose payload is null, which is a payload', () => {
    const frame = parseServerFrame(JSON.stringify({
      type: 'event',
      channel: CHANNEL,
      event: 'deleted',
      payload: null,
    }))

    expect(frame).toEqual({
      type: 'event',
      channel: CHANNEL,
      event: 'deleted',
      payload: null,
    })
  })

  it('should read the acknowledgements', () => {
    expect(parseServerFrame(JSON.stringify({ type: 'subscribed', channel: CHANNEL })))
      .toEqual({ type: 'subscribed', channel: CHANNEL })
    expect(parseServerFrame(JSON.stringify({ type: 'unsubscribed', channel: CHANNEL })))
      .toEqual({ type: 'unsubscribed', channel: CHANNEL })
  })

  it('should read an error, with or without a channel', () => {
    expect(parseServerFrame(JSON.stringify({
      type: 'error',
      channel: CHANNEL,
      code: 'not_found',
      message: 'No such channel.',
    }))).toEqual({
      type: 'error',
      channel: CHANNEL,
      code: 'not_found',
      message: 'No such channel.',
    })

    expect(parseServerFrame(JSON.stringify({
      type: 'error',
      code: 'invalid_frame',
      message: 'That is not a frame this server understands.',
    }))).toEqual({
      type: 'error',
      channel: undefined,
      code: 'invalid_frame',
      message: 'That is not a frame this server understands.',
    })
  })

  it('should keep an error code it has no branch for', () => {
    const frame = parseServerFrame(JSON.stringify({
      type: 'error',
      code: 'a_code_from_a_newer_server',
      message: 'Something new happened.',
    }))

    expect(frame?.type).toBe('error')
    expect(frame).toMatchObject({ message: 'Something new happened.' })
  })

  it('should refuse anything that is not a frame', () => {
    expect(parseServerFrame('not json')).toBeNull()
    expect(parseServerFrame('[]')).toBeNull()
    expect(parseServerFrame('"a string"')).toBeNull()
    expect(parseServerFrame('null')).toBeNull()
    expect(parseServerFrame(new ArrayBuffer(4))).toBeNull()
    expect(parseServerFrame(undefined)).toBeNull()
  })

  it('should refuse a frame of a type it does not know', () => {
    expect(parseServerFrame(JSON.stringify({ type: 'published', channel: CHANNEL }))).toBeNull()
    expect(parseServerFrame(JSON.stringify({ channel: CHANNEL }))).toBeNull()
  })

  it('should refuse a frame missing the fields its type promises', () => {
    expect(parseServerFrame(JSON.stringify({ type: 'subscribed' }))).toBeNull()
    expect(parseServerFrame(JSON.stringify({ type: 'event', channel: CHANNEL }))).toBeNull()
    expect(parseServerFrame(JSON.stringify({ type: 'event', event: 'created' }))).toBeNull()
    expect(parseServerFrame(JSON.stringify({ type: 'error', code: 'not_found' }))).toBeNull()
  })
})
