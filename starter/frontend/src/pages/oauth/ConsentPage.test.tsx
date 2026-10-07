// Copyright © 2026 Jalapeno Labs

// UI
import { ConsentPage } from './ConsentPage'
import { ConsentErrorPage } from './ConsentErrorPage'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ME = {
  user: {
    id: '3f1a2b3c-0000-4000-8000-0000000000ff',
    email: 'ada@example.com',
    email_verified: true,
    first_name: 'Ada',
    last_name: 'Lovelace',
    time_zone: 'UTC',
    locale: 'en-US',
    platform_roles: [],
    created_at: '2026-10-05T00:00:00Z',
    avatar_version: null,
  },
}

const REQUEST = {
  id: 'request-1',
  client: {
    name: 'Claude Code',
    client_id: 'https://claude.ai/oauth/claude-code-client-metadata',
    kind: 'metadata_document',
    verified_host: 'claude.ai',
    client_uri: 'https://claude.ai',
  },
  redirect_host: 'localhost',
  redirect_is_loopback: true,
  scopes: [{ name: 'samples:read', description: 'Browse the sample library' }],
  expires_at: '2026-10-06T00:10:00Z',
}

const CALLBACK = 'http://localhost:53682/callback?code=abc&state=xyz&iss=http%3A%2F%2Flocalhost'

/**
 * Answers the consent screen's three requests, telling a decision from a read
 * by method, which the shared table-by-path stub cannot.
 */
function stubConsent(decisions: unknown[], decision: Response = Response.json({ redirect_to: CALLBACK })) {
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const request = input instanceof Request
      ? input
      : new Request(String(input))
    const { pathname } = new URL(request.url, 'http://localhost')

    if (pathname === '/auth/me') {
      return Response.json(ME)
    }
    if (pathname === '/oauth/requests/request-1' && request.method === 'GET') {
      return Response.json(REQUEST)
    }
    if (pathname === '/oauth/requests/request-1' && request.method === 'POST') {
      decisions.push(await request.json())
      return decision
    }
    return new Response('{}', { status: 404 })
  })
}

describe('ConsentPage', () => {
  it('should name the client, who vouches for it, where it goes, and what it asks', async () => {
    stubConsent([])

    renderWithProviders(<ConsentPage requestId='request-1' />, '/consent?request=request-1')

    expect(await screen.findByText('Connect Claude Code')).toBeTruthy()
    expect(screen.getByText('Published by claude.ai, which vouches for this app\'s name.'))
      .toBeTruthy()
    expect(screen.getByText('After you decide, you go back to localhost.')).toBeTruthy()
    // A code bound for this device is a code any program here could have asked for.
    expect(screen.getByText(/That address is a program on this computer/)).toBeTruthy()
    expect(screen.getByText('Browse the sample library')).toBeTruthy()
    expect(await screen.findByText('You are signed in as ada@example.com.')).toBeTruthy()
  })

  it('should record an approval and send the browser to the client', async () => {
    const decisions: unknown[] = []
    stubConsent(decisions)
    const assign = vi.fn()
    vi.stubGlobal('location', { ...window.location, assign })

    renderWithProviders(<ConsentPage requestId='request-1' />, '/consent?request=request-1')
    fireEvent.click(await screen.findByRole('button', { name: 'Allow' }))

    await waitFor(() => expect(assign).toHaveBeenCalledWith(CALLBACK))
    expect(decisions).toEqual([{ approve: true }])
  })

  it('should say a request that expired while open is gone, not "Not found."', async () => {
    stubConsent([], Response.json({ message: 'Not found.' }, { status: 404 }))

    renderWithProviders(<ConsentPage requestId='request-1' />, '/consent?request=request-1')
    fireEvent.click(await screen.findByRole('button', { name: 'Allow' }))

    expect(await screen.findByText(/This request expired or was already answered/)).toBeTruthy()
    expect(screen.queryByText('Not found.')).toBeNull()
  })

  it('should send an account on a temporary password to choose its own', async () => {
    stubFetch({
      '/auth/me': { status: 200, body: { user: { ...ME.user, password_change_required: true }}},
      '/oauth/requests/request-1': {
        status: 403,
        body: {
          message: 'Choose a new password before you continue.',
          code: 'password_change_required',
        },
      },
    })

    renderWithProviders(<ConsentPage requestId='request-1' />, '/consent?request=request-1')

    expect(await screen.findByText(/Your account is on a temporary password/)).toBeTruthy()
    const link = screen.getByRole('link', { name: 'Choose a new password' })
    expect(link.getAttribute('href')).toBe('/settings/security')
    expect(screen.getByText('Connect an app')).toBeTruthy()
  })

  it('should say a request is gone rather than that something broke', async () => {
    stubFetch({
      '/auth/me': { status: 200, body: ME },
      '/oauth/requests/request-1': { status: 404, body: { message: 'Not found.' }},
    })

    renderWithProviders(<ConsentPage requestId='request-1' />, '/consent?request=request-1')

    expect(await screen.findByText(/This request expired or was already answered/)).toBeTruthy()
  })
})

describe('ConsentErrorPage', () => {
  it('should explain a known code and fall back for an unknown one', () => {
    const { unmount } = renderWithProviders(<ConsentErrorPage code='invalid_redirect_uri' />)
    expect(screen.getByText(/asked to send you somewhere it never registered/)).toBeTruthy()
    unmount()

    renderWithProviders(<ConsentErrorPage code='something_new' />)
    expect(screen.getByText(/could not be completed/)).toBeTruthy()
  })
})
