// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '..'

// Misc
import { appClient, toSearchParams } from '..'

/**
 * A peripheral notion as the backend serializes it.
 *
 * Wire field names stay snake_case: they are the contract, not a preference.
 */
export type PeripheralNotion = {
  id: string
  team_id: string
  name: string
  created_at: string
  updated_at: string
}

/** The permission model key, matching `config/roles.yml`. */
export const PERIPHERAL_NOTION_MODEL = 'PeripheralNotion'

type ListPeripheralNotionsResponse = {
  peripheral_notions: PeripheralNotion[]
  pagination: Pagination
}

export function listPeripheralNotions(teamId: string, query: ListQuery = {}) {
  return appClient
    .get(`teams/${teamId}/peripheral-notions`, {
      searchParams: toSearchParams(query),
    })
    .json<ListPeripheralNotionsResponse>()
}

type PeripheralNotionResponse = {
  peripheral_notion: PeripheralNotion
}

type CreatePeripheralNotionRequest = {
  name: string
}

export function createPeripheralNotion(teamId: string, body: CreatePeripheralNotionRequest) {
  return appClient
    .post(`teams/${teamId}/peripheral-notions`, { json: body })
    .json<PeripheralNotionResponse>()
}
