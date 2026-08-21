// Copyright © 2026 Jalapeno Labs

import type { ListQuery, Pagination } from '..'

// Misc
import { appClient, toSearchParams } from '..'

/**
 * One field's move, as the backend records it.
 *
 * `unknown` on both sides because a change set spans every column of every
 * model: the screen renders them as JSON rather than pretending to know.
 */
export type AuditChange = {
  old: unknown
  new: unknown
}

/**
 * One recorded act, as `anubis::audit::AuditEvent` serializes it.
 *
 * Wire field names stay snake_case: they are the contract, not a preference.
 *
 * `actor_name` and `subject_label` are copies taken at the moment of the act,
 * so a deleted account and a destroyed record still read as themselves. Either
 * can be null: a system act has no actor, and an act on nothing nameable has
 * no label.
 */
export type AuditEvent = {
  id: string
  team_id: string | null
  organization_id: string | null
  user_id: string | null
  actor_name: string | null
  /** `created` / `updated` / `destroyed`, or a framework verb like `team.renamed`. */
  action: string
  subject_type: string
  subject_id: string | null
  subject_label: string | null
  changes: Record<string, AuditChange>
  request_id: string | null
  created_at: string
}

type ListAuditEventsQuery = ListQuery & {
  /** Only events about this model, e.g. `CreativeConcept` or `Team`. */
  subject_type?: string
  /** Only events by this user. */
  actor_id?: string
}

type ListAuditEventsResponse = {
  audit_events: AuditEvent[]
  pagination: Pagination
}

export function listTeamAuditEvents(teamId: string, query: ListAuditEventsQuery = {}) {
  return appClient
    .get(`teams/${teamId}/audit-events`, { searchParams: toSearchParams(query) })
    .json<ListAuditEventsResponse>()
}
