// Copyright © 2026 Jalapeno Labs

/**
 * One event published on a channel.
 *
 * `payload` is whatever the server published, so narrow it where you consume
 * it. The channel it arrived on is carried along, which is what lets one
 * socket serve many channels.
 */
export type RealtimeEvent = {
  channel: string
  event: string
  payload: unknown
}

/**
 * Why the server refused a request, or degraded a subscription.
 *
 * `not_found` covers both a channel that does not exist and one the signed-in
 * user may not listen to; the server answers them identically on purpose.
 */
export type RealtimeErrorCode =
  | 'invalid_frame'
  | 'not_found'
  | 'too_many_channels'
  | 'lagged'
  | 'internal'

/** A frame the server sends. */
export type ServerFrame =
  | { type: 'subscribed', channel: string }
  | { type: 'unsubscribed', channel: string }
  | { type: 'event', channel: string, event: string, payload: unknown }
  | { type: 'error', channel?: string, code: RealtimeErrorCode, message: string }

/** A frame the client sends. */
export type ClientFrame =
  | { type: 'subscribe', channel: string }
  | { type: 'unsubscribe', channel: string }

/**
 * Reads one server frame off the wire, or null when it is not one.
 *
 * The socket is a runtime boundary: anything can arrive on it, including a
 * frame from a newer server this client does not understand. Everything
 * unrecognized becomes null rather than an exception, so one bad frame never
 * takes the connection down with it.
 */
export function parseServerFrame(data: unknown): ServerFrame | null {
  if (typeof data !== 'string') {
    console.debug('parseServerFrame received a non-text frame', data)
    return null
  }

  let parsed: unknown
  try {
    parsed = JSON.parse(data)
  }
  catch (error) {
    console.debug('parseServerFrame received a frame that is not JSON', { data, error })
    return null
  }

  if (!parsed || typeof parsed !== 'object') {
    console.debug('parseServerFrame received a frame that is not an object', parsed)
    return null
  }

  // The one unavoidable assertion: JSON.parse returns `any` shape, and every
  // field below is checked before it is used.
  const frame = parsed as Record<string, unknown>

  if (frame.type === 'subscribed' || frame.type === 'unsubscribed') {
    if (typeof frame.channel !== 'string') {
      console.debug('parseServerFrame received an acknowledgement without a channel', frame)
      return null
    }
    return { type: frame.type, channel: frame.channel }
  }

  if (frame.type === 'event') {
    if (typeof frame.channel !== 'string' || typeof frame.event !== 'string') {
      console.debug('parseServerFrame received an event without a channel or name', frame)
      return null
    }
    return {
      type: 'event',
      channel: frame.channel,
      event: frame.event,
      payload: frame.payload,
    }
  }

  if (frame.type === 'error') {
    if (typeof frame.code !== 'string' || typeof frame.message !== 'string') {
      console.debug('parseServerFrame received an error without a code or message', frame)
      return null
    }
    return {
      type: 'error',
      channel: typeof frame.channel === 'string'
        ? frame.channel
        : undefined,
      // Codes are an open set: a newer server may send one this client has no
      // branch for, and the message beside it still reads.
      code: frame.code as RealtimeErrorCode,
      message: frame.message,
    }
  }

  console.debug('parseServerFrame received a frame of an unknown type', frame)
  return null
}
