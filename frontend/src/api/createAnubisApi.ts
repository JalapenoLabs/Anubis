// Copyright © 2026 Jalapeno Labs

import type { Credentials, MessageEnvelope, User, WireUserEnvelope } from './types'

// Utility
import ky from 'ky'

// Misc
import { toUser } from './types'

type AnubisApiOptions = {
  /** Path or URL the auth routes are mounted under. Defaults to same-origin. */
  prefix?: string
}

/**
 * Builds the typed client for the framework's auth endpoints.
 *
 * Sessions ride on HttpOnly cookies, so no tokens are handled here; the
 * browser sends the cookie automatically on same-origin requests.
 */
export function createAnubisApi(options: AnubisApiOptions = {}) {
  const client = ky.create({
    prefix: options.prefix ?? '/',
    // Auth outcomes (401, 409, 400) are modeled responses, not retryable
    // transport failures.
    retry: 0,
  })

  async function register(credentials: Credentials): Promise<User> {
    const response = await client
      .post('auth/register', { json: credentials })
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  async function login(credentials: Credentials): Promise<User> {
    const response = await client
      .post('auth/login', { json: credentials })
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  async function logout(): Promise<void> {
    await client.post('auth/logout')
  }

  async function me(): Promise<User> {
    const response = await client
      .get('auth/me')
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  function requestEmailVerification() {
    return client
      .post('auth/verify-email/request')
      .json<MessageEnvelope>()
  }

  async function confirmEmailVerification(token: string): Promise<User> {
    const response = await client
      .post('auth/verify-email/confirm', { json: { token }})
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  function requestPasswordReset(email: string) {
    return client
      .post('auth/password-reset/request', { json: { email }})
      .json<MessageEnvelope>()
  }

  function confirmPasswordReset(token: string, password: string) {
    return client
      .post('auth/password-reset/confirm', { json: { token, password }})
      .json<MessageEnvelope>()
  }

  return {
    register,
    login,
    logout,
    me,
    requestEmailVerification,
    confirmEmailVerification,
    requestPasswordReset,
    confirmPasswordReset,
  } as const
}

export type AnubisApi = ReturnType<typeof createAnubisApi>
