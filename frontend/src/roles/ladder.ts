// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'

// Misc
import { outranks } from './permissions'

/**
 * Whether `viewer` may move `role` on the ladder.
 *
 * The administrator role is pinned to the top and the default role to the
 * bottom, and nobody moves a role at or above their own rank.
 */
export function isMovable(viewer: TenantAccess, role: Role): boolean {
  return !role.isDefault && !role.isAdministrator && outranks(viewer, role)
}

/**
 * The ladder with `roleId` moved to `toIndex`, or `null` when the server would
 * refuse it.
 *
 * `roles` is the whole ladder, highest first. A move is allowed when every
 * role the viewer may not move keeps its place, which is exactly the server's
 * rule: roles at or above the viewer stay where they are, so whatever moves
 * stays below them.
 */
export function moveRole(
  roles: readonly Role[],
  viewer: TenantAccess,
  roleId: string,
  toIndex: number,
): Role[] | null {
  const fromIndex = roles.findIndex((role) => role.id === roleId)
  if (fromIndex < 0 || toIndex < 0 || toIndex >= roles.length || fromIndex === toIndex) {
    return null
  }
  if (!isMovable(viewer, roles[fromIndex])) {
    return null
  }

  const next = [ ...roles ]
  const [ moved ] = next.splice(fromIndex, 1)
  next.splice(toIndex, 0, moved)

  const pinnedStayed = roles.every((role, index) => {
    return isMovable(viewer, role) || next[index].id === role.id
  })
  return pinnedStayed
    ? next
    : null
}

/** The ids a reorder request names: every role but the two pinned ones, highest first. */
export function reorderRequest(ladder: readonly Role[]): string[] {
  const ids: string[] = []
  for (const role of ladder) {
    if (!role.isDefault && !role.isAdministrator) {
      ids.push(role.id)
    }
  }
  return ids
}
