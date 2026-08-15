// Copyright © 2026 Jalapeno Labs

// Utility
import { HTTPError } from 'ky'

/**
 * The machine-readable codes an Anubis error body can carry.
 *
 * Only a refusal a client has to *act* on carries one; everything else answers
 * with a message alone. Branch on these rather than on the message, which
 * translation is free to rewrite.
 */
export const ApiErrorCode = {
  /** An administrator disabled the account. Nothing the browser does helps. */
  accountDisabled: 'account_disabled',
  /** The account owes a password change before it may do anything else. */
  passwordChangeRequired: 'password_change_required',
} as const

export type ApiErrorCodeValue = typeof ApiErrorCode[keyof typeof ApiErrorCode]

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
 * Extracts the refusal's stable code, when the backend named one.
 *
 * Returns null for every error that carries only a message, which is most of
 * them, and for codes shipped by a backend newer than this client: an unknown
 * code is one this client has no screen for, so it belongs in the message's
 * fallback path rather than in a branch.
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
