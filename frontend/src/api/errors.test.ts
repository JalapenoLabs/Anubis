// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, describe, expect, it, vi } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import {
  IMAGE_REFUSED,
  IMAGE_SCREEN_UNAVAILABLE,
  PASSWORD_CHANGE_REQUIRED,
  getApiErrorCode,
  getApiErrorMessage,
  getRetryAfterSeconds,
} from './errors'

const RATE_LIMITED_BODY = { message: 'Too many requests. Try again in 30 seconds.' }

/**
 * The error a real request produces, so these helpers meet ky's own shape.
 *
 * Constructing an `HTTPError` by hand would prove only that the constructor
 * was called with what the assertion expects; going through the client is what
 * proves the header and the body survive the trip.
 */
async function failedRequest(
  status: number,
  headers: Record<string, string> = {},
  body: Record<string, unknown> = RATE_LIMITED_BODY,
) {
  vi.stubGlobal('fetch', async () => {
    return new Response(JSON.stringify(body), {
      status,
      headers: {
        'content-type': 'application/json',
        ...headers,
      },
    })
  })

  try {
    await ky.post('http://localhost/auth/login', { retry: 0 })
  }
  catch (error) {
    return error
  }

  throw new Error(`a ${status} response must reject`)
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('getRetryAfterSeconds', () => {
  it('should read the wait a 429 names', async () => {
    const error = await failedRequest(429, { 'retry-after': '30' })

    expect(getRetryAfterSeconds(error)).toBe(30)
  })

  it('should ignore the header on any other status', async () => {
    const error = await failedRequest(503, { 'retry-after': '30' })

    expect(getRetryAfterSeconds(error)).toBeNull()
  })

  it('should answer null for a 429 whose header is missing or unusable', async () => {
    expect(getRetryAfterSeconds(await failedRequest(429))).toBeNull()
    expect(getRetryAfterSeconds(await failedRequest(429, { 'retry-after': '0' }))).toBeNull()
    expect(getRetryAfterSeconds(await failedRequest(429, { 'retry-after': '1.5' }))).toBeNull()
    expect(
      getRetryAfterSeconds(await failedRequest(429, { 'retry-after': 'Wed, 21 Oct 2026 07:28:00 GMT' })),
    ).toBeNull()
  })

  it('should answer null for anything that is not an HTTP error', () => {
    expect(getRetryAfterSeconds(new Error('the network went away'))).toBeNull()
    expect(getRetryAfterSeconds(null)).toBeNull()
  })
})

describe('getApiErrorMessage', () => {
  it('should read the backend message a rate-limited response carries', async () => {
    const error = await failedRequest(429, { 'retry-after': '30' })

    expect(getApiErrorMessage(error)).toBe('Too many requests. Try again in 30 seconds.')
  })
})

describe('getApiErrorCode', () => {
  it('should read the code a refusal names beside its message', async () => {
    const error = await failedRequest(403, {}, {
      message: 'Choose a new password before you continue.',
      code: PASSWORD_CHANGE_REQUIRED,
    })

    expect(getApiErrorCode(error)).toBe(PASSWORD_CHANGE_REQUIRED)
    expect(getApiErrorMessage(error)).toBe('Choose a new password before you continue.')
  })

  it('should answer null for a refusal that names no code', async () => {
    expect(getApiErrorCode(await failedRequest(409, {}, { message: 'taken' }))).toBeNull()
  })

  it('should answer null for anything that is not an HTTP error', () => {
    expect(getApiErrorCode(new Error('the network went away'))).toBeNull()
  })

  it('should read the code a screened image upload is refused with', async () => {
    const refused = await failedRequest(400, {}, {
      message: 'That image can\'t be used. Choose a different picture.',
      code: IMAGE_REFUSED,
    })
    const unscreened = await failedRequest(503, {}, {
      message: 'We couldn\'t check that image just now. Try again in a moment.',
      code: IMAGE_SCREEN_UNAVAILABLE,
    })

    expect(getApiErrorCode(refused)).toBe(IMAGE_REFUSED)
    expect(getApiErrorCode(unscreened)).toBe(IMAGE_SCREEN_UNAVAILABLE)
  })

  it('should keep the codes the backend sends', () => {
    expect(PASSWORD_CHANGE_REQUIRED).toBe('password_change_required')
    expect(IMAGE_REFUSED).toBe('image_refused')
    expect(IMAGE_SCREEN_UNAVAILABLE).toBe('image_screen_unavailable')
  })
})
