// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '..'

// Misc
import { appClient, toSearchParams } from '..'

/**
 * A creative concept as the backend serializes it.
 *
 * Wire field names stay snake_case: they are the contract, not a preference.
 */
export type CreativeConcept = {
  id: string
  team_id: string
  name: string
  created_at: string
  updated_at: string
}

/** The permission model key, matching `config/roles.yml`. */
export const CREATIVE_CONCEPT_MODEL = 'CreativeConcept'

type ListCreativeConceptsQuery = ListQuery & {
  /** Case-insensitive substring match on the name. */
  name?: string
}

type ListCreativeConceptsResponse = {
  creative_concepts: CreativeConcept[]
  pagination: Pagination
}

export function listCreativeConcepts(teamId: string, query: ListCreativeConceptsQuery = {}) {
  return appClient
    .get(`teams/${teamId}/creative-concepts`, { searchParams: toSearchParams(query) })
    .json<ListCreativeConceptsResponse>()
}

type CreativeConceptResponse = {
  creative_concept: CreativeConcept
}

export function getCreativeConcept(creativeConceptId: string) {
  return appClient
    .get(`creative-concepts/${creativeConceptId}`)
    .json<CreativeConceptResponse>()
}

export function createCreativeConcept(teamId: string, body: { name: string }) {
  return appClient
    .post(`teams/${teamId}/creative-concepts`, { json: body })
    .json<CreativeConceptResponse>()
}

export function updateCreativeConcept(creativeConceptId: string, body: { name?: string }) {
  return appClient
    .patch(`creative-concepts/${creativeConceptId}`, { json: body })
    .json<CreativeConceptResponse>()
}

export function deleteCreativeConcept(creativeConceptId: string) {
  return appClient.delete(`creative-concepts/${creativeConceptId}`)
}
