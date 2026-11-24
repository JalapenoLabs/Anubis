// Copyright © 2026 Jalapeno Labs

import type { GrantTier, Role, RoleSummary, TenantAccess } from '../api/types'

/**
 * The framework's own permission subjects, spelled as `roles.yml` and the
 * server spell them.
 *
 * Namespaced so that no scaffolded model can share a name with one: an
 * application's own `Subscription` model never collides with the framework's.
 */
export const MEMBERSHIP_SUBJECT = 'Tenancy::Membership'
export const ROLE_SUBJECT = 'Tenancy::Role'
export const SUBSCRIPTION_SUBJECT = 'Billing::Subscription'

export type PermissionAction = 'read' | 'create' | 'update' | 'destroy'

/** The actions in the order every screen lists them. */
export const PERMISSION_ACTIONS: readonly PermissionAction[] = [ 'read', 'create', 'update', 'destroy' ]

/** One subject's permission keys, as the role editor lays them out in a row. */
export type PermissionGroup = {
  subject: string
  /** The keys the subject takes, by action; an action it does not take is absent. */
  keys: Partial<Record<PermissionAction, string>>
}

/** Why a role cannot be granted by somebody, or `null` when it can. */
export type AssignabilityRefusal = 'default' | 'wrong-tier' | 'outranked' | 'lacks-permissions'

/** Whether `access` allows `action` on `subject`. */
export function allows(
  access: TenantAccess | undefined,
  action: PermissionAction,
  subject: string,
): boolean {
  return Boolean(access?.permissions.includes(`${subject}.${action}`))
}

/**
 * Whether `viewer` may act on `role` at all: edit it, grant it, revoke it, or
 * delete it.
 *
 * The server's rule, mirrored so a screen draws only what it would accept:
 * the administrator is exempt from the ladder, nobody else touches the
 * administrator role, and everybody else acts only below their own rank.
 */
export function outranks(
  viewer: TenantAccess,
  role: Pick<Role, 'position' | 'isAdministrator'>,
): boolean {
  if (viewer.isAdministrator) {
    return true
  }
  if (role.isAdministrator) {
    return false
  }
  return role.position < viewer.rank
}

/** Whether `viewer` holds every one of `permissions` where they stand. */
export function holdsAll(viewer: TenantAccess, permissions: readonly string[]): boolean {
  if (viewer.isAdministrator) {
    return true
  }
  return permissions.every((permission) => viewer.permissions.includes(permission))
}

/**
 * Why `viewer` may not hand `role` to somebody at `tier`, or `null` when they may.
 *
 * The same questions the server asks of a grant or an invitation: the default
 * role is everybody's already, the role must be grantable at that tier, it
 * must sit below the viewer, and it must carry nothing the viewer does not
 * hold.
 */
export function assignability(
  viewer: TenantAccess,
  role: Role,
  tier: GrantTier,
): AssignabilityRefusal | null {
  if (role.isDefault) {
    return 'default'
  }
  if (!role.grantableAt.includes(tier)) {
    return 'wrong-tier'
  }
  if (!outranks(viewer, role)) {
    return 'outranked'
  }
  if (!holdsAll(viewer, role.permissions)) {
    return 'lacks-permissions'
  }
  return null
}

/**
 * Whether `viewer` may change the roles of somebody holding `held`.
 *
 * Changing your own is held to the role rules alone, since a role below you
 * whose permissions you hold gives you nothing you lack.
 */
export function outranksMember(
  viewer: TenantAccess,
  held: readonly RoleSummary[],
  isSelf: boolean,
): boolean {
  if (isSelf || viewer.isAdministrator) {
    return true
  }
  const highest = held.reduce((rank, role) => Math.max(rank, role.position), 0)
  return highest < viewer.rank
}

/**
 * Lays a permission vocabulary out by subject, one row per subject.
 *
 * Subjects keep the order the vocabulary gives them, which is sorted, so the
 * role editor's rows read the same way every time.
 */
export function groupPermissions(keys: readonly string[]): PermissionGroup[] {
  const groups: PermissionGroup[] = []
  const bySubject = new Map<string, PermissionGroup>()

  for (const key of keys) {
    const separator = key.lastIndexOf('.')
    if (separator <= 0) {
      console.debug('groupPermissions skipped a key with no subject', key)
      continue
    }

    const subject = key.slice(0, separator)
    const action = key.slice(separator + 1)
    if (!isPermissionAction(action)) {
      console.debug('groupPermissions skipped a key with an unknown action', key)
      continue
    }

    let group = bySubject.get(subject)
    if (!group) {
      group = { subject, keys: {}}
      bySubject.set(subject, group)
      groups.push(group)
    }
    group.keys[action] = key
  }

  return groups
}

function isPermissionAction(candidate: string): candidate is PermissionAction {
  return PERMISSION_ACTIONS.some((action) => action === candidate)
}
