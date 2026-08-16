// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, describe, expect, it, vi } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import { getApiErrorMessage, getRetryAfterSeconds } from './errors'

/**
 * The error a real request produces, so these helpers meet ky's own shape.
 *
 * Constructing an `HTTPError` by hand would prove only that the constructor
 * was called with what the assertion expects; going through the client is what
 * proves the header and the body survive the trip.
 */
async function failedRequest(status: number, headers: Record<string, string> = {}) {
  vi.stubGlobal('fetch', async () => {
    return new Response(JSON.stringify({ message: 'Too many requests. Try again in 30 seconds.' }), {
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
