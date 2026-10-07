// Copyright © 2026 Jalapeno Labs

// Core
import { PASSWORD_CHANGE_REQUIRED, getApiErrorCode } from '@jalapenolabs/anubis'

// Utility
import { HTTPError } from 'ky'

/**
 * The translation key for a consent refusal that has a sentence of its own.
 *
 * Reading the request and deciding it fail the same two ways a person can act
 * on: the request expired or was already answered (`404`), or the account is
 * on a temporary password and must choose its own first (`403` with
 * `PASSWORD_CHANGE_REQUIRED`). Anything else answers null, and the caller
 * shows the server's message, because "Not found." or "Something went wrong"
 * would tell the person nothing about what to do next.
 */
export function consentRefusalKey(error: unknown) {
  if (error instanceof HTTPError && error.response.status === 404) {
    return 'oauth.consent.expired'
  }

  if (getApiErrorCode(error) === PASSWORD_CHANGE_REQUIRED) {
    return 'oauth.consent.passwordChangeRequired'
  }

  return null
}
