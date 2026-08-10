// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '..'

// Misc
import { appClient, toSearchParams } from '..'

/**
 * A tangible thing as the backend serializes it.
 *
 * Wire field names stay snake_case: they are the contract, not a preference.
 */
export type TangibleThing = {
  id: string
  creative_concept_id: string
  name: string
  description: string | null
  created_at: string
  updated_at: string
}

/** The permission model key, matching `config/roles.yml`. */
export const TANGIBLE_THING_MODEL = 'TangibleThing'

type ListTangibleThingsQuery = ListQuery & {
  /** Case-insensitive substring match on the name. */
  name?: string
}

type ListTangibleThingsResponse = {
  tangible_things: TangibleThing[]
  pagination: Pagination
}

export function listTangibleThings(
  creativeConceptId: string,
  query: ListTangibleThingsQuery = {},
) {
  return appClient
    .get(`creative-concepts/${creativeConceptId}/tangible-things`, {
      searchParams: toSearchParams(query),
    })
    .json<ListTangibleThingsResponse>()
}

type TangibleThingResponse = {
  tangible_thing: TangibleThing
}

type CreateTangibleThingRequest = {
  name: string
  description?: string
}

export function createTangibleThing(
  creativeConceptId: string,
  body: CreateTangibleThingRequest,
) {
  return appClient
    .post(`creative-concepts/${creativeConceptId}/tangible-things`, { json: body })
    .json<TangibleThingResponse>()
}

type UpdateTangibleThingRequest = {
  name?: string
  /** A blank description clears the column. */
  description?: string
  /** Moves the thing to another concept of the same team. */
  creative_concept_id?: string
}

export function updateTangibleThing(
  tangibleThingId: string,
  body: UpdateTangibleThingRequest,
) {
  return appClient
    .patch(`tangible-things/${tangibleThingId}`, { json: body })
    .json<TangibleThingResponse>()
}

export function deleteTangibleThing(tangibleThingId: string) {
  return appClient.delete(`tangible-things/${tangibleThingId}`)
}
