// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'

// Core
import { describe, expect, it } from 'vitest'

// Misc
import {
  allows,
  assignability,
  groupPermissions,
  holdsAll,
  outranks,
  outranksMember,
} from './permissions'

function role(overrides: Partial<Role>): Role {
  return {
    id: 'role',
    key: null,
    name: 'Role',
    color: '#000000',
    position: 1,
    permissions: [],
    customized: true,
    isDefault: false,
    isAdministrator: false,
    ...overrides,
  }
}

const moderator: TenantAccess = {
  roles: [{ id: 'moderator', name: 'Moderator', color: '#dc2626', position: 3 }],
  permissions: [ 'Tenancy::Membership.update', 'Project.read' ],
  rank: 3,
  isAdministrator: false,
}

const administrator: TenantAccess = {
  roles: [{ id: 'admin', name: 'Administrator', color: '#e11d48', position: 4 }],
  permissions: [],
  rank: 4,
  isAdministrator: true,
}

describe('allows', () => {
  it('reads one permission key out of an access', () => {
    expect(allows(moderator, 'update', 'Tenancy::Membership')).toBe(true)
    expect(allows(moderator, 'destroy', 'Tenancy::Membership')).toBe(false)
    expect(allows(undefined, 'read', 'Project')).toBe(false)
  })
})

describe('outranks', () => {
  it('admits only roles below the viewer', () => {
    expect(outranks(moderator, role({ position: 2 }))).toBe(true)
    expect(outranks(moderator, role({ position: 3 }))).toBe(false)
    expect(outranks(moderator, role({ position: 4, isAdministrator: true }))).toBe(false)
  })

  it('exempts the administrator, the administrator role included', () => {
    expect(outranks(administrator, role({ position: 4, isAdministrator: true }))).toBe(true)
  })
})

describe('holdsAll', () => {
  it('needs every permission, unless the viewer administers', () => {
    expect(holdsAll(moderator, [ 'Project.read' ])).toBe(true)
    expect(holdsAll(moderator, [ 'Project.read', 'Project.update' ])).toBe(false)
    expect(holdsAll(administrator, [ 'Project.update' ])).toBe(true)
  })
})

describe('assignability', () => {
  it('names why a role cannot be handed out', () => {
    expect(assignability(moderator, role({ isDefault: true, position: 0 }))).toBe('default')
    expect(assignability(moderator, role({ position: 3 }))).toBe('outranked')
    expect(assignability(moderator, role({ position: 1, permissions: [ 'Project.update' ]})))
      .toBe('lacks-permissions')
    expect(assignability(moderator, role({ position: 1, permissions: [ 'Project.read' ]})))
      .toBeNull()
  })
})

describe('outranksMember', () => {
  it('measures a member by the highest role they hold', () => {
    const junior = [{ id: 'helper', name: 'Helper', color: '#000000', position: 1 }]
    const peer = [{ id: 'moderator', name: 'Moderator', color: '#dc2626', position: 3 }]

    expect(outranksMember(moderator, junior, false)).toBe(true)
    expect(outranksMember(moderator, peer, false)).toBe(false)
    expect(outranksMember(moderator, peer, true)).toBe(true)
    expect(outranksMember(moderator, [], false)).toBe(true)
  })
})

describe('groupPermissions', () => {
  it('lays keys out one subject per row, keeping the given order', () => {
    const groups = groupPermissions([
      'Billing::Subscription.update',
      'Project.read',
      'Project.destroy',
      'not a key',
      'Project.manage',
    ])

    expect(groups).toEqual([
      { subject: 'Billing::Subscription', keys: { update: 'Billing::Subscription.update' }},
      { subject: 'Project', keys: { read: 'Project.read', destroy: 'Project.destroy' }},
    ])
  })
})
