// Copyright © 2026 Jalapeno Labs

/**
 * The has-many-through association `IncidentalLinkage` carries: the peripheral
 * notions a creative concept is linked to.
 *
 * A join model owns no page, so these functions are its whole frontend
 * surface. They serve the association's own UI and the
 * `peripheral_notion_ids` field `anubis scaffold field` adds to the creative
 * concept's form, which is why the options hook lives beside them.
 */

import type { FieldOption } from '@jalapenolabs/anubis'
import type { PeripheralNotion } from './peripheralNotionRoutes'

// Core
import useSWR from 'swr'

// Misc
import { appClient } from '..'

/** A stable empty list, so an unresolved fetch never re-renders the field. */
const NO_OPTIONS: FieldOption[] = []

type IncidentalLinkageOptionsResponse = {
  options: FieldOption[]
}

export function listIncidentalLinkageOptions(teamId: string) {
  return appClient
    .get(`teams/${teamId}/incidental-linkages/options`)
    .json<IncidentalLinkageOptionsResponse>()
}

/**
 * The options a `super_select` field offers for this association.
 *
 * The endpoint scopes them to the caller's team through the join model's
 * `valid_*` method, so a form can only ever offer records the caller may link.
 */
export function useIncidentalLinkageOptions(teamId: string): FieldOption[] {
  const options = useSWR(
    [ 'incidental-linkage-options', teamId ],
    () => listIncidentalLinkageOptions(teamId),
  )
  return options.data?.options ?? NO_OPTIONS
}

type AttachedPeripheralNotionsResponse = {
  peripheral_notions: PeripheralNotion[]
}

export function listCreativeConceptPeripheralNotions(creativeConceptId: string) {
  return appClient
    .get(`creative-concepts/${creativeConceptId}/peripheral-notions`)
    .json<AttachedPeripheralNotionsResponse>()
}

export function attachPeripheralNotion(creativeConceptId: string, peripheralNotionId: string) {
  return appClient.post(`creative-concepts/${creativeConceptId}/peripheral-notions`, {
    json: { peripheral_notion_id: peripheralNotionId },
  })
}

export function detachPeripheralNotion(creativeConceptId: string, peripheralNotionId: string) {
  return appClient.delete(
    `creative-concepts/${creativeConceptId}/peripheral-notions/${peripheralNotionId}`,
  )
}
