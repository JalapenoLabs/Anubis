// Copyright © 2026 Jalapeno Labs

// UI
import { ConnectedClientsCard } from './ConnectedClientsCard'

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
    first_name: null,
    last_name: null,
    time_zone: 'UTC',
    locale: 'en-US',
    platform_roles: [],
    created_at: '2026-10-05T00:00:00Z',
    avatar_version: null,
  },
}

const CONNECTION = {
  id: 'grant-1',
  client: {
    name: 'Claude Code',
    client_id: 'https://claude.ai/oauth/claude-code-client-metadata',
    kind: 'metadata_document',
    verified_host: 'claude.ai',
    client_uri: 'https://claude.ai',
  },
  scopes: [{ name: 'samples:read', description: 'Browse the sample library' }],
  created_at: '2026-10-01T00:00:00Z',
  last_used_at: '2026-10-05T00:00:00Z',
}

describe('ConnectedClientsCard', () => {
  it('should say so when nothing is connected', async () => {
    stubFetch({
      '/auth/me': { status: 200, body: ME },
      '/oauth/connections': { status: 200, body: { connections: []}},
    })

    renderWithProviders(<ConnectedClientsCard />)

    expect(await screen.findByText(/No apps are connected/)).toBeTruthy()
  })

  it('should say the list failed rather than that nothing is connected', async () => {
    stubFetch({
      '/auth/me': { status: 200, body: ME },
      '/oauth/connections': { status: 500, body: { message: 'Something went wrong.' }},
    })

    renderWithProviders(<ConnectedClientsCard />)

    expect(await screen.findByText(/could not be loaded/)).toBeTruthy()
    expect(screen.queryByText(/No apps are connected/)).toBeNull()
  })

  it('should list a connection and revoke it before it disappears', async () => {
    let connections = [ CONNECTION ]
    const revoked: string[] = []
    vi.stubGlobal('fetch', async (input: Request | string | URL) => {
      const request = input instanceof Request
        ? input
        : new Request(String(input))
      const { pathname } = new URL(request.url, 'http://localhost')

      if (pathname === '/auth/me') {
        return Response.json(ME)
      }
      if (pathname === '/oauth/connections') {
        return Response.json({ connections })
      }
      if (pathname === '/oauth/connections/grant-1' && request.method === 'DELETE') {
        revoked.push('grant-1')
        connections = []
        return new Response(null, { status: 204 })
      }
      return new Response('{}', { status: 404 })
    })

    renderWithProviders(<ConnectedClientsCard />)

    expect(await screen.findByText('Claude Code')).toBeTruthy()
    expect(screen.getByText('Published by claude.ai')).toBeTruthy()
    expect(screen.getByText('samples:read')).toBeTruthy()

    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }))

    await waitFor(() => expect(screen.queryByText('Claude Code')).toBeNull())
    expect(revoked).toEqual([ 'grant-1' ])
  })
})
