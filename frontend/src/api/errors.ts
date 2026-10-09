// Copyright © 2026 Jalapeno Labs

// Utility
import { HTTPError } from 'ky'

/** The one status the framework answers with a `Retry-After` header. */
const TOO_MANY_REQUESTS = 429

/**
 * The code a `403` carries when the account must change its password first.
 *
 * An operator set a temporary password, and until the account chooses its
 * own, every authenticated route but `me`, changing the password, and signing
 * out refuses it with this. Match on it with `getApiErrorCode` and send the
 * person to the change-password screen; the message is copy and may change,
 * the code will not.
 */
export const PASSWORD_CHANGE_REQUIRED = 'password_change_required'

/**
 * The code a `400` carries when the application's image screen refused an
 * upload, such as an avatar.
 *
 * Nothing was stored and the previous picture stays. The message says only
 * that the image can't be used, never why, so show it as it is and offer to
 * choose another picture rather than retrying the same one.
 */
export const IMAGE_REFUSED = 'image_refused'

/**
 * The code a `503` carries when the image screen failed to decide.
 *
 * Nothing was stored, because a screen that could not look has not said yes.
 * The failure is the server's, so the same picture may succeed if the person
 * tries again in a moment.
 */
export const IMAGE_SCREEN_UNAVAILABLE = 'image_screen_unavailable'

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
 * Extracts the backend's machine-readable error code from a failed request.
 *
 * Most Anubis errors carry only a message. The ones a client must react to by
 * kind, such as `PASSWORD_CHANGE_REQUIRED`, also carry a stable `code`, and
 * this is the part of the body to branch on. Returns null when the error is
 * not an HTTP error or names no code.
 */
export function getApiErrorCode(error: unknown): string | null {
  if (!(error instanceof HTTPError)) {
    console.debug('getApiErrorCode received a non-HTTP error', error)
    return null
  }

  const data: unknown = error.data
  if (
    data
    && typeof data === 'object'
    && 'code' in data
    && typeof data.code === 'string'
  ) {
    return data.code
  }

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
