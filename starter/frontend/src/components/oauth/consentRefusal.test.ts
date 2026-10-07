// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, describe, expect, it, vi } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import { consentRefusalKey } from './consentRefusal'

/**
 * The error a real refused request produces, so the mapping meets ky's own
 * shape rather than one built by hand to match the assertion.
 */
async function refusal(status: number, body: Record<string, string>) {
  vi.stubGlobal('fetch', async () => Response.json(body, { status }))
  try {
    await ky.get('http://localhost/oauth/requests/request-1', { retry: 0 })
  }
  catch (error) {
    return error
  }
  throw new Error(`a ${status} must reject`)
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('consentRefusalKey', () => {
  it('should read a 404 as a request that expired or was answered', async () => {
    const error = await refusal(404, { message: 'Not found.' })

    expect(consentRefusalKey(error)).toBe('oauth.consent.expired')
  })

  it('should read a coded 403 as an account that must choose a password', async () => {
    const error = await refusal(403, {
      message: 'Choose a new password before you continue.',
      code: 'password_change_required',
    })

    expect(consentRefusalKey(error)).toBe('oauth.consent.passwordChangeRequired')
  })

  it('should leave every other failure to the server message', async () => {
    expect(consentRefusalKey(await refusal(403, { message: 'Forbidden.' }))).toBeNull()
    expect(consentRefusalKey(await refusal(500, { message: 'Something went wrong.' }))).toBeNull()
    expect(consentRefusalKey(new Error('offline'))).toBeNull()
  })
})
