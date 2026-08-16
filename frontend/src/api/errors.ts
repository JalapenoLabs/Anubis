// Copyright © 2026 Jalapeno Labs

// Utility
import { HTTPError } from 'ky'

/** The one status the framework answers with a `Retry-After` header. */
const TOO_MANY_REQUESTS = 429

/**
 * Extracts the backend's user-safe error message from a failed request.
 *
 * Anubis endpoints answer errors as `{"message": "..."}`, which ky pre-parses
 * onto `HTTPError.data`. Returns null when the error is not an HTTP error or
 * carries no message, so callers fall back to their own copy.
 */
export function getApiErrorMessage(error: unknown): string | null {
  if (!(error instanceof HTTPError)) {
    console.debug('getApiErrorMessage received a non-HTTP error', error)
    return null
  }

  const data: unknown = error.data
  if (
    data
    && typeof data === 'object'
    && 'message' in data
    && typeof data.message === 'string'
  ) {
    return data.message
  }

  console.debug('getApiErrorMessage found no message in the error body', data)
  return null
}

/**
 * Extracts the wait, in whole seconds, a rate-limited request must observe.
 *
 * Anubis answers `429 Too Many Requests` with a `Retry-After` header carrying
 * whole seconds, never an HTTP date, so a caller reads the wait rather than
 * guessing one. A non-null result is also how a caller tells a rate-limited
 * failure from every other one: only the `429` contract produces it.
 *
 * Returns null for any other error, for a `429` whose header a proxy stripped,
 * and for a header that is not a positive integer. Callers then fall back to
 * the message `getApiErrorMessage` reads, which already names the wait in
 * words.
 */
export function getRetryAfterSeconds(error: unknown): number | null {
  if (!(error instanceof HTTPError)) {
    console.debug('getRetryAfterSeconds received a non-HTTP error', error)
    return null
  }

  if (error.response.status !== TOO_MANY_REQUESTS) {
    return null
  }

  const header = error.response.headers.get('retry-after')
  const seconds = Number(header)
  if (!Number.isInteger(seconds) || seconds <= 0) {
    console.debug('getRetryAfterSeconds found no usable Retry-After header', header)
    return null
  }

  return seconds
}
