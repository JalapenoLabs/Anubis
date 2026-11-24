// Copyright © 2026 Jalapeno Labs

/**
 * One event published on a channel.
 *
 * `payload` is whatever the server published, typically the changed record in
 * the shape the REST API answers with, so narrow it where you consume it.
 * `id` places the event in the deployment's one sequence; the channel it
 * arrived on is carried along, which is what lets one stream serve many.
 */
export type RealtimeEvent = {
  type: 'event'
  id: string
  channel: string
  event: string
  payload: unknown
}

/**
 * What the client holds for `channel` may be stale: read it again.
 *
 * A channel opens with one, because anything fetched before the stream was
 * live may already be behind, and gets one whenever a reconnect asked for
 * events the server no longer keeps. Either way the answer is the same:
 * revalidate whatever the channel describes.
 */
export type RealtimeReset = {
  type: 'reset'
  channel: string
}

/** What a channel's listener hears: an event, or a reset. */
export type RealtimeMessage = RealtimeEvent | RealtimeReset

/**
 * Why a stream will not carry a channel it was asked for.
 *
 * `not_found` covers both a channel that does not exist and one the signed-in
 * user may not listen to; the server answers them identically on purpose.
 */
export type RealtimeErrorCode =
  | 'invalid_channel'
  | 'not_found'
  | 'too_many_channels'

/** A channel the stream refused, and why. */
export type RealtimeRefusal = {
  type: 'error'
  channel: string
  code: RealtimeErrorCode
  message: string
}

/**
 * The stream is live: everything before this was delivered, or reset.
 *
 * `id` is the cursor a reconnect resumes from when nothing else has arrived.
 */
export type RealtimeLive = {
  type: 'live'
  id: string
}

/** One frame a stream carries. */
export type StreamFrame =
  | RealtimeEvent
  | RealtimeReset
  | RealtimeRefusal
  | RealtimeLive

/**
 * Reads one frame off the wire, or null when it is not one.
 *
 * `data` is an event's data field and `id` its id field, when it had one. The
 * stream is a runtime boundary: anything can arrive on it, including a frame
 * from a newer server this client does not understand. Everything
 * unrecognized becomes null rather than an exception, so one bad frame never
 * takes the stream down with it.
 */
export function parseStreamFrame(data: string, id: string | undefined): StreamFrame | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(data)
  }
  catch (error) {
    console.debug('parseStreamFrame received a frame that is not JSON', { data, error })
    return null
  }

  if (!parsed || typeof parsed !== 'object') {
    console.debug('parseStreamFrame received a frame that is not an object', parsed)
    return null
  }

  // The one unavoidable assertion: JSON.parse returns `any` shape, and every
  // field below is checked before it is used.
  const frame = parsed as Record<string, unknown>

  if (frame.type === 'event') {
    if (!id || typeof frame.channel !== 'string' || typeof frame.event !== 'string') {
      console.debug('parseStreamFrame received an event without an id, channel, or name', frame)
      return null
    }
    return {
      type: 'event',
      id,
      channel: frame.channel,
      event: frame.event,
      payload: frame.payload,
    }
  }

  if (frame.type === 'reset') {
    if (typeof frame.channel !== 'string') {
      console.debug('parseStreamFrame received a reset without a channel', frame)
      return null
    }
    return { type: 'reset', channel: frame.channel }
  }

  if (frame.type === 'live') {
    if (!id) {
      console.debug('parseStreamFrame received a live frame without its cursor', frame)
      return null
    }
    return { type: 'live', id }
  }

  if (frame.type === 'error') {
    if (
      typeof frame.channel !== 'string'
      || typeof frame.code !== 'string'
      || typeof frame.message !== 'string'
    ) {
      console.debug('parseStreamFrame received a refusal without a channel, code, or message', frame)
      return null
    }
    return {
      type: 'error',
      channel: frame.channel,
      // Codes are an open set: a newer server may send one this client has no
      // branch for, and the message beside it still reads.
      code: frame.code as RealtimeErrorCode,
      message: frame.message,
    }
  }

  console.debug('parseStreamFrame received a frame of an unknown type', frame)
  return null
}
