// Copyright © 2026 Jalapeno Labs

import type {
  AuthorizationDelivery,
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

type WireDelivery =
  | { kind: 'redirect', host: string, is_loopback: boolean }
  | { kind: 'device' }

type WireAuthorizationRequest = {
  id: string
  client: WireClient
  delivery: WireDelivery
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

function toDelivery(wire: WireDelivery): AuthorizationDelivery {
  if (wire.kind === 'device') {
    return { kind: 'device' }
  }

  return {
    kind: 'redirect',
    host: wire.host,
    isLoopback: wire.is_loopback,
  }
}

/**
 * The account's side of the OAuth authorization server.
 *
 * The code-entry page turns a device's user code into a request, the consent
 * screen reads a request and records the person's decision, and the settings
 * screen lists the clients they connected and revokes one. The protocol
 * endpoints themselves (`/oauth/authorize`, `/oauth/device_authorization`,
 * `/oauth/token`) belong to the connecting program, never to the browser, so
 * nothing here calls them.
 */
export function createOauthRoutes(client: KyInstance) {
  /**
   * The request a device's user code names, answering its id for the consent
   * screen.
   *
   * Every code that names nothing waiting, whether mistyped, expired, or
   * already decided, answers the same `400`, and a code entered too often
   * answers `429` with the wait.
   */
  async function verifyUserCode(userCode: string): Promise<string> {
    const response = await client
      .post('oauth/device_verification', { json: { user_code: userCode }})
      .json<{ request_id: string }>()
    return response.request_id
  }

  /** What the person is being asked to approve. `404` once it expired or was decided. */
  async function getAuthorizationRequest(requestId: string): Promise<AuthorizationRequest> {
    const wire = await client
      .get(`oauth/requests/${requestId}`)
      .json<WireAuthorizationRequest>()
    return {
      id: wire.id,
      client: toClient(wire.client),
      delivery: toDelivery(wire.delivery),
      scopes: wire.scopes,
      expiresAt: wire.expires_at,
    }
  }

  /**
   * Approves or denies a request, answering where to send the browser.
   *
   * For a redirecting client the destination is its redirect URI carrying a
   * code or an error, a different program's address; for a device it is this
   * application's code-entry page, saying how it went. The caller navigates
   * there with `window.location.assign` either way.
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
    verifyUserCode,
    getAuthorizationRequest,
    decideAuthorizationRequest,
    listConnectedClients,
    revokeConnectedClient,
  } as const
}
