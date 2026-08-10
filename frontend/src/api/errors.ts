// Copyright © 2026 Jalapeno Labs

// Utility
import { HTTPError } from 'ky'

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
