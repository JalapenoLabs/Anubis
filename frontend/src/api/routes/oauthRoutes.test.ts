// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import { createOauthRoutes } from './oauthRoutes'

const WIRE_CLIENT = {
  name: 'Claude Code',
  client_id: 'https://claude.ai/oauth/claude-code-client-metadata',
  kind: 'metadata_document',
  verified_host: 'claude.ai',
  client_uri: 'https://claude.ai',
}

type Recorded = {
  method: string
  url: string
  body: unknown
}

/** The OAuth routes over a fetch that records each request and answers `answer`. */
function recordingRoutes(answer: unknown, status = 200) {
  const sent: Recorded[] = []
  const client = ky.create({
    prefix: 'http://localhost/',
    retry: 0,
    fetch: async (input) => {
      const request = input instanceof Request
        ? input
        : new Request(input)
      const text = await request.text()
      sent.push({
        method: request.method,
        url: request.url,
        body: text
          ? JSON.parse(text)
          : null,
      })
      const body = status === 204
        ? null
        : JSON.stringify(answer)
      return new Response(body, {
        status,
        headers: { 'content-type': 'application/json' },
      })
    },
  })

  return { routes: createOauthRoutes(client), sent }
}

describe('getAuthorizationRequest', () => {
  it('should read the request into the shape the consent screen renders', async () => {
    const { routes, sent } = recordingRoutes({
      id: 'request-1',
      client: WIRE_CLIENT,
      delivery: { kind: 'redirect', host: 'localhost', is_loopback: true },
      scopes: [{ name: 'samples:read', description: 'Browse samples' }],
      expires_at: '2026-10-06T00:10:00Z',
    })

    const request = await routes.getAuthorizationRequest('request-1')

    expect(sent[0].url).toBe('http://localhost/oauth/requests/request-1')
    expect(request).toEqual({
      id: 'request-1',
      client: {
        name: 'Claude Code',
        clientId: 'https://claude.ai/oauth/claude-code-client-metadata',
        kind: 'metadata_document',
        verifiedHost: 'claude.ai',
        clientUri: 'https://claude.ai',
      },
      delivery: { kind: 'redirect', host: 'localhost', isLoopback: true },
      scopes: [{ name: 'samples:read', description: 'Browse samples' }],
      expiresAt: '2026-10-06T00:10:00Z',
    })
  })

  it('should read a device request as one with nowhere to redirect', async () => {
    const { routes } = recordingRoutes({
      id: 'request-2',
      client: { ...WIRE_CLIENT, name: 'Server agent' },
      delivery: { kind: 'device' },
      scopes: [],
      expires_at: '2026-10-06T00:10:00Z',
    })

    const request = await routes.getAuthorizationRequest('request-2')

    expect(request.delivery).toEqual({ kind: 'device' })
  })
})

describe('verifyUserCode', () => {
  it('should send the code as typed and answer the request it names', async () => {
    const { routes, sent } = recordingRoutes({ request_id: 'request-2' })

    const requestId = await routes.verifyUserCode('wdjb-mjht')

    expect(sent[0]).toEqual({
      method: 'POST',
      url: 'http://localhost/oauth/device_verification',
      body: { user_code: 'wdjb-mjht' },
    })
    expect(requestId).toBe('request-2')
  })
})

describe('decideAuthorizationRequest', () => {
  it('should send the decision and answer where the browser goes next', async () => {
    const destination = 'http://localhost:53682/callback?code=abc&state=xyz&iss=https%3A%2F%2Fapp'
    const { routes, sent } = recordingRoutes({ redirect_to: destination })

    const redirectTo = await routes.decideAuthorizationRequest('request-1', false)

    expect(sent[0]).toEqual({
      method: 'POST',
      url: 'http://localhost/oauth/requests/request-1',
      body: { approve: false },
    })
    expect(redirectTo).toBe(destination)
  })
})

describe('listConnectedClients', () => {
  it('should read every connection with its client and scopes', async () => {
    const { routes } = recordingRoutes({
      connections: [{
        id: 'grant-1',
        client: { ...WIRE_CLIENT, kind: 'dynamic', verified_host: null },
        scopes: [],
        created_at: '2026-10-01T00:00:00Z',
        last_used_at: '2026-10-05T00:00:00Z',
      }],
    })

    const connections = await routes.listConnectedClients()

    expect(connections).toHaveLength(1)
    expect(connections[0].id).toBe('grant-1')
    expect(connections[0].client.kind).toBe('dynamic')
    expect(connections[0].client.verifiedHost).toBeNull()
    expect(connections[0].lastUsedAt).toBe('2026-10-05T00:00:00Z')
  })
})

describe('revokeConnectedClient', () => {
  it('should delete the connection by its grant id', async () => {
    const { routes, sent } = recordingRoutes(null, 204)

    await routes.revokeConnectedClient('grant-1')

    expect(sent[0].method).toBe('DELETE')
    expect(sent[0].url).toBe('http://localhost/oauth/connections/grant-1')
  })
})
