// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '..'

// Misc
import { appClient, toSearchParams } from '..'

/**
 * A granular detail as the backend serializes it.
 *
 * Wire field names stay snake_case: they are the contract, not a preference.
 */
export type GranularDetail = {
  id: string
  tangible_thing_id: string
  name: string
  description: string | null
  // 🐺 anubis:wire-fields
  created_at: string
  updated_at: string
}

/** The permission model key, matching `config/roles.yml`. */
export const GRANULAR_DETAIL_MODEL = 'GranularDetail'

type ListGranularDetailsQuery = ListQuery & {
  /** Case-insensitive substring match on the name. */
  name?: string
}

type ListGranularDetailsResponse = {
  granular_details: GranularDetail[]
  pagination: Pagination
}

export function listGranularDetails(
  tangibleThingId: string,
  query: ListGranularDetailsQuery = {},
) {
  return appClient
    .get(`tangible-things/${tangibleThingId}/granular-details`, {
      searchParams: toSearchParams(query),
    })
    .json<ListGranularDetailsResponse>()
}

type GranularDetailResponse = {
  granular_detail: GranularDetail
}

export function getGranularDetail(granularDetailId: string) {
  return appClient
    .get(`granular-details/${granularDetailId}`)
    .json<GranularDetailResponse>()
}

type CreateGranularDetailRequest = {
  name: string
  description?: string
  // 🐺 anubis:create-request
}

export function createGranularDetail(
  tangibleThingId: string,
  body: CreateGranularDetailRequest,
) {
  return appClient
    .post(`tangible-things/${tangibleThingId}/granular-details`, { json: body })
    .json<GranularDetailResponse>()
}

type UpdateGranularDetailRequest = {
  name?: string
  /** A blank description clears the column. */
  description?: string
  /** Moves the detail to another thing of the same team. */
  tangible_thing_id?: string
  // 🐺 anubis:update-request
}

export function updateGranularDetail(
  granularDetailId: string,
  body: UpdateGranularDetailRequest,
) {
  return appClient
    .patch(`granular-details/${granularDetailId}`, { json: body })
    .json<GranularDetailResponse>()
}

export function deleteGranularDetail(granularDetailId: string) {
  return appClient.delete(`granular-details/${granularDetailId}`)
}

// 🐺 anubis:route-functions
