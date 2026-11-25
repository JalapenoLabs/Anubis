// Copyright © 2026 Jalapeno Labs

import type { ResourceGrant, ResourceGrantChange } from '../api/types'

/** What one team reaches of one kind, as the access screen offers it. */
export type ResourceGrantMode = 'nothing' | 'every' | 'only'

/**
 * One team's grants of one kind while they are being edited.
 *
 * The picked records keep their names, so the chips render without waiting
 * on a picker page that may not hold them, and they survive switching to
 * "every" and back, so trying the broader choice costs no picks.
 */
export type ResourceGrantDraft = {
  mode: ResourceGrantMode
  resources: { value: string, label: string }[]
}

/** The draft a grant reads as; a team with no grant reaches nothing. */
export function draftOf(grant: ResourceGrant | undefined): ResourceGrantDraft {
  if (!grant) {
    return { mode: 'nothing', resources: []}
  }
  if (grant.every) {
    return { mode: 'every', resources: []}
  }
  if (!grant.resources.length) {
    return { mode: 'nothing', resources: []}
  }
  return { mode: 'only', resources: grant.resources }
}

/**
 * The end state a draft asks the server for.
 *
 * "Only these" with nothing picked yet is nothing, which is exactly what the
 * server would keep, so the screen never sends a choice it cannot read back.
 */
export function changeOf(draft: ResourceGrantDraft): ResourceGrantChange {
  if (draft.mode === 'every') {
    return { every: true, resourceIds: []}
  }
  if (draft.mode === 'nothing') {
    return { every: false, resourceIds: []}
  }
  return {
    every: false,
    resourceIds: draft.resources.map((resource) => resource.value),
  }
}

/** Whether two drafts ask the server for the same end state, in any order. */
export function isSameGrant(left: ResourceGrantDraft, right: ResourceGrantDraft): boolean {
  const before = changeOf(left)
  const after = changeOf(right)
  if (before.every !== after.every) {
    return false
  }

  const kept = new Set(before.resourceIds)
  return before.resourceIds.length === after.resourceIds.length
    && after.resourceIds.every((id) => kept.has(id))
}
