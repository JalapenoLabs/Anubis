// Copyright © 2026 Jalapeno Labs

import type { AnubisApi } from '../api/createAnubisApi'
import type { User } from '../api/types'
import type { ReactNode } from 'react'

// Core
import { describe, expect, it, vi } from 'vitest'

// Utility
import { act, renderHook, waitFor } from '@testing-library/react'
import ky from 'ky'
import { SWRConfig } from 'swr'

// Misc
import { AnubisProvider } from './AnubisProvider'
import { useCurrentUser } from './useCurrentUser'
import { useInvitation } from './useInvitation'

const INVITEE: User = {
  id: 'an-invitee',
  email: 'invitee@example.com',
  emailVerified: true,
  firstName: null,
  lastName: null,
  timeZone: 'UTC',
  locale: 'en-US',
  createdAt: '2026-10-06T00:00:00Z',
  avatarVersion: null,
  platformRoles: [ 'operator' ],
  passwordChangeRequired: false,
}

const PREVIEW = {
  email: 'invitee@example.com',
  expiresAt: '2026-10-07T00:00:00Z',
}

/** The error ky raises for a real response of `status`. */
async function httpError(status: number) {
  const client = ky.create({
    retry: 0,
    fetch: async () => new Response(JSON.stringify({ message: 'refused' }), {
      status,
      headers: { 'content-type': 'application/json' },
    }),
  })

  try {
    await client.post('http://localhost/auth/invitations/lookup')
  }
  catch (error) {
    return error
  }

  throw new Error(`a ${status} response must reject`)
}

/**
 * A double for the parts of the client the hook calls.
 *
 * The cast is the seam a test double always needs: the hook takes the whole
 * client from its provider, and this is the slice of it under test.
 */
function fakeApi(overrides: Partial<AnubisApi>): AnubisApi {
  return {
    me: vi.fn().mockRejectedValue(new Error('signed out')),
    lookupInvitation: vi.fn().mockResolvedValue(PREVIEW),
    acceptInvitation: vi.fn().mockResolvedValue(INVITEE),
    ...overrides,
  } as unknown as AnubisApi
}

/** Renders the hook beside `useCurrentUser`, under fresh providers. */
function renderInvitation(api: AnubisApi, token: string | null) {
  function Harness(props: { children: ReactNode }) {
    return <SWRConfig value={{ provider: () => new Map() }}>
      <AnubisProvider api={api}>{
        props.children
      }</AnubisProvider>
    </SWRConfig>
  }

  return renderHook(() => ({
    invitation: useInvitation(token),
    currentUser: useCurrentUser(),
  }), { wrapper: Harness })
}

describe('useInvitation', () => {
  it('should read the address a live link was sent to', async () => {
    const api = fakeApi({})

    const { result } = renderInvitation(api, 'a-token')

    await waitFor(() => {
      expect(result.current.invitation.invitation).toEqual(PREVIEW)
    })
    expect(result.current.invitation.isInvalid).toBe(false)
    expect(api.lookupInvitation).toHaveBeenCalledWith('a-token')
  })

  it('should call a refused link invalid rather than an error', async () => {
    const refusal = await httpError(400)
    const api = fakeApi({ lookupInvitation: vi.fn().mockRejectedValue(refusal) })

    const { result } = renderInvitation(api, 'a-spent-token')

    await waitFor(() => {
      expect(result.current.invitation.isInvalid).toBe(true)
    })
    expect(result.current.invitation.error).toBeUndefined()
    expect(result.current.invitation.invitation).toBeNull()
  })

  it('should keep a server failure as an error worth retrying', async () => {
    const outage = await httpError(503)
    const api = fakeApi({ lookupInvitation: vi.fn().mockRejectedValue(outage) })

    const { result } = renderInvitation(api, 'a-token')

    await waitFor(() => {
      expect(result.current.invitation.error).toBe(outage)
    })
    expect(result.current.invitation.isInvalid).toBe(false)
  })

  it('should call a link with no token invalid without asking the backend', () => {
    const api = fakeApi({})

    const { result } = renderInvitation(api, null)

    expect(result.current.invitation.isInvalid).toBe(true)
    expect(api.lookupInvitation).not.toHaveBeenCalled()
  })

  it('should sign the new account in where useCurrentUser can see it', async () => {
    const api = fakeApi({})

    const { result } = renderInvitation(api, 'a-token')

    await act(async () => {
      await result.current.invitation.accept({ password: 'correct horse battery staple' })
    })

    expect(api.acceptInvitation).toHaveBeenCalledWith('a-token', {
      password: 'correct horse battery staple',
    })
    await waitFor(() => {
      expect(result.current.currentUser.user).toEqual(INVITEE)
    })
  })
})
