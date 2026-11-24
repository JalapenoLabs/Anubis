// Copyright © 2026 Jalapeno Labs

import type { StreamFrame } from './protocol'

// Core
import { describe, expect, expectTypeOf, it } from 'vitest'

// Misc
import { parseStreamFrame } from './protocol'

const CHANNEL = 'team:11111111-1111-1111-1111-111111111111:projects'
const ID = '3f2b9c0d1e4a5b6c-42'

describe('parseStreamFrame', () => {
  it('should read an event, taking its id from the SSE id field', () => {
    const frame = parseStreamFrame(JSON.stringify({
      type: 'event',
      channel: CHANNEL,
      event: 'created',
      payload: { id: 7, name: 'Apollo' },
    }), ID)

    expect(frame).toEqual({
      type: 'event',
      id: ID,
      channel: CHANNEL,
      event: 'created',
      payload: { id: 7, name: 'Apollo' },
    })
  })

  it('should read an event whose payload is null, which is a payload', () => {
    const frame = parseStreamFrame(JSON.stringify({
      type: 'event',
      channel: CHANNEL,
      event: 'deleted',
      payload: null,
    }), ID)

    expect(frame).toEqual({
      type: 'event',
      id: ID,
      channel: CHANNEL,
      event: 'deleted',
      payload: null,
    })
  })

  it('should refuse an event without an id, which could never be resumed after', () => {
    const frame = parseStreamFrame(JSON.stringify({
      type: 'event',
      channel: CHANNEL,
      event: 'created',
      payload: {},
    }), undefined)

    expect(frame).toBeNull()
  })

  it('should read a reset and a refusal', () => {
    expect(parseStreamFrame(JSON.stringify({ type: 'reset', channel: CHANNEL }), undefined))
      .toEqual({ type: 'reset', channel: CHANNEL })

    expect(parseStreamFrame(JSON.stringify({
      type: 'error',
      channel: CHANNEL,
      code: 'not_found',
      message: 'No such channel.',
    }), undefined)).toEqual({
      type: 'error',
      channel: CHANNEL,
      code: 'not_found',
      message: 'No such channel.',
    })
  })

  it('should read the live frame with the cursor its id carries', () => {
    expect(parseStreamFrame(JSON.stringify({ type: 'live' }), ID))
      .toEqual({ type: 'live', id: ID })
    expect(parseStreamFrame(JSON.stringify({ type: 'live' }), undefined)).toBeNull()
  })

  it('should keep a refusal code it does not know, since the message still reads', () => {
    const frame = parseStreamFrame(JSON.stringify({
      type: 'error',
      channel: CHANNEL,
      code: 'a_code_from_the_future',
      message: 'Something new.',
    }), undefined)

    expect(frame).toMatchObject({ type: 'error', code: 'a_code_from_the_future' })
  })

  it('should turn anything that is not a frame into null rather than throwing', () => {
    for (const data of [
      'not json at all',
      'null',
      '42',
      '"a string"',
      JSON.stringify({}),
      JSON.stringify({ type: 'subscribed', channel: CHANNEL }),
      JSON.stringify({ type: 'event', event: 'created' }),
      JSON.stringify({ type: 'reset' }),
      JSON.stringify({ type: 'error', channel: CHANNEL, code: 7, message: 'x' }),
    ]) {
      expect(parseStreamFrame(data, ID)).toBeNull()
    }
  })

  it('should type its answer as a stream frame or nothing', () => {
    expectTypeOf(parseStreamFrame('{}', undefined)).toEqualTypeOf<StreamFrame | null>()
  })
})
