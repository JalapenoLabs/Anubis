// Copyright © 2026 Jalapeno Labs

import type {
  AuthorizationRequest,
  ConnectedClient,
  OauthClient,
  OauthScope,
} from '../types'
import type { KyInstance } from 'ky'

type WireClient = {
  name: string
  client_id: string
  kind: OauthClient['kind']
  verified_host: string | null
  client_uri: string | null
}

type WireAuthorizationRequest = {
  id: string
  client: WireClient
  redirect_host: string
  redirect_is_loopback: boolean
  scopes: OauthScope[]
  expires_at: string
}

type WireConnection = {
  id: string
  client: WireClient
  scopes: OauthScope[]
  created_at: string
  last_used_at: string
}

function toClient(wire: WireClient): OauthClient {
  return {
    name: wire.name,
    clientId: wire.client_id,
    kind: wire.kind,
    verifiedHost: wire.verified_host,
    clientUri: wire.client_uri,
  }
}

/**
 * The account's side of the OAuth authorization server.
 *
 * The consent screen reads a request and records the person's decision; the
 * settings screen lists the clients they connected and revokes one. The
 * protocol endpoints themselves (`/oauth/authorize`, `/oauth/token`) belong to
 * the connecting program, never to the browser, so nothing here calls them.
 */
export function createOauthRoutes(client: KyInstance) {
  /** What the person is being asked to approve. `404` once it expired or was decided. */
  async function getAuthorizationRequest(requestId: string): Promise<AuthorizationRequest> {
    const wire = await client
      .get(`oauth/requests/${requestId}`)
      .json<WireAuthorizationRequest>()
    return {
      id: wire.id,
      client: toClient(wire.client),
      redirectHost: wire.redirect_host,
      redirectIsLoopback: wire.redirect_is_loopback,
      scopes: wire.scopes,
      expiresAt: wire.expires_at,
    }
  }

  /**
   * Approves or denies a request, answering where to send the browser.
   *
   * The destination is the client's redirect URI carrying a code or an error,
   * so the caller navigates there with `window.location.assign`; it is a
   * different program's address, not a route in this application.
   */
  async function decideAuthorizationRequest(requestId: string, approve: boolean): Promise<string> {
    const response = await client
      .post(`oauth/requests/${requestId}`, { json: { approve }})
      .json<{ redirect_to: string }>()
    return response.redirect_to
  }

  async function listConnectedClients(): Promise<ConnectedClient[]> {
    const response = await client
      .get('oauth/connections')
      .json<{ connections: WireConnection[] }>()
    return response.connections.map((connection) => ({
      id: connection.id,
      client: toClient(connection.client),
      scopes: connection.scopes,
      createdAt: connection.created_at,
      lastUsedAt: connection.last_used_at,
    }))
  }

  /** Ends a connection: its tokens stop working on the client's next request. */
  async function revokeConnectedClient(connectionId: string): Promise<void> {
    await client.delete(`oauth/connections/${connectionId}`)
  }

  return {
    getAuthorizationRequest,
    decideAuthorizationRequest,
    listConnectedClients,
    revokeConnectedClient,
  } as const
}
