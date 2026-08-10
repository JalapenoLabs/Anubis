// Copyright © 2026 Jalapeno Labs

// Utility
import { HTTPError } from 'ky'

/**
 * Extracts the backend's user-safe error message from a failed request.
 *
 * Anubis endpoints answer errors as `{"message": "..."}`. Returns null when
 * the error is not an HTTP error or carries no parseable message, so callers
 * fall back to their own copy.
 */
export async function getApiErrorMessage(error: unknown): Promise<string | null> {
  if (!(error instanceof HTTPError)) {
    console.debug('getApiErrorMessage received a non-HTTP error', error)
    return null
  }

  try {
    const body: { message?: string } = await error.response.clone().json()
    return body.message ?? null
  }
  catch (parseError) {
    console.debug('getApiErrorMessage could not parse the error body', parseError)
    return null
  }
}
