// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '../index'

// Misc
import { developersClient, toSearchParams } from '../index'

/** Where a delivery stands; mirrors `anubis::webhooks::DeliveryStatus`. */
export type WebhookDeliveryStatus = 'pending' | 'delivered' | 'failed' | 'dead'

/** A team's subscription. The signing secret is never part of this shape. */
export type WebhookEndpoint = {
  id: string
  team_id: string
  url: string
  description: string | null
  event_types: string[]
  active: boolean
  created_at: string
  updated_at: string
}

/** One event's journey to one endpoint, with its attempt history. */
export type WebhookDelivery = {
  id: string
  webhook_endpoint_id: string
  event_type: string
  payload: unknown
  status: WebhookDeliveryStatus
  attempts: number
  response_status: number | null
  last_error: string | null
  delivered_at: string | null
  created_at: string
  updated_at: string
}

type ListEndpointsResponse = {
  webhook_endpoints: WebhookEndpoint[]
}

export function listWebhookEndpoints(teamId: string) {
  return developersClient
    .get(`teams/${teamId}/webhook-endpoints`)
    .json<ListEndpointsResponse>()
}

type CreateEndpointRequest = {
  url: string
  description?: string
  event_types: string[]
}

type CreateEndpointResponse = {
  webhook_endpoint: WebhookEndpoint
  /** Returned exactly once, at creation. Only its sealed form is stored. */
  secret: string
}

export function createWebhookEndpoint(teamId: string, body: CreateEndpointRequest) {
  return developersClient
    .post(`teams/${teamId}/webhook-endpoints`, { json: body })
    .json<CreateEndpointResponse>()
}

type UpdateEndpointRequest = {
  url?: string
  /** Blank clears the description; absent leaves it alone. */
  description?: string
  event_types?: string[]
  active?: boolean
}

type EndpointResponse = {
  webhook_endpoint: WebhookEndpoint
}

export function updateWebhookEndpoint(
  teamId: string,
  endpointId: string,
  body: UpdateEndpointRequest,
) {
  return developersClient
    .patch(`teams/${teamId}/webhook-endpoints/${endpointId}`, { json: body })
    .json<EndpointResponse>()
}

export function deleteWebhookEndpoint(teamId: string, endpointId: string) {
  return developersClient.delete(`teams/${teamId}/webhook-endpoints/${endpointId}`)
}

type ListDeliveriesResponse = {
  webhook_deliveries: WebhookDelivery[]
  pagination: Pagination
}

export function listWebhookDeliveries(
  teamId: string,
  endpointId: string,
  query: ListQuery = {},
) {
  return developersClient
    .get(`teams/${teamId}/webhook-endpoints/${endpointId}/deliveries`, {
      searchParams: toSearchParams(query),
    })
    .json<ListDeliveriesResponse>()
}

type DeliveryResponse = {
  webhook_delivery: WebhookDelivery
}

/** Queues a fresh attempt, leaving the original delivery's history intact. */
export function redeliverWebhookDelivery(
  teamId: string,
  endpointId: string,
  deliveryId: string,
) {
  return developersClient
    .post(
      `teams/${teamId}/webhook-endpoints/${endpointId}/deliveries/${deliveryId}/redeliver`,
    )
    .json<DeliveryResponse>()
}
