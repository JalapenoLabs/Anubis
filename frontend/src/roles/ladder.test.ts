// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { isMovable, moveRole, reorderRequest } from './ladder'

function role(id: string, position: number, overrides: Partial<Role> = {}): Role {
  return {
    id,
    key: null,
    name: id,
    color: '#000000',
    position,
    permissions: [],
    customized: true,
    isDefault: false,
    isAdministrator: false,
    grantableAt: [ 'organization', 'sub_tenant', 'team' ],
    ...overrides,
  }
}

/** Highest first: admin 4, moderator 3, billing 2, editor 1, default 0. */
const ladder: Role[] = [
  role('admin', 4, { isAdministrator: true }),
  role('moderator', 3),
  role('billing', 2),
  role('editor', 1),
  role('default', 0, { isDefault: true }),
]

const moderator: TenantAccess = {
  roles: [{ id: 'moderator', name: 'Moderator', color: '#000000', position: 3 }],
  permissions: [],
  rank: 3,
  isAdministrator: false,
}

const administrator: TenantAccess = { ...moderator, rank: 4, isAdministrator: true }

function ids(roles: Role[] | null) {
  return roles?.map((entry) => entry.id) ?? null
}

describe('isMovable', () => {
  it('pins the two structural roles and everything at or above the viewer', () => {
    expect(ladder.map((entry) => isMovable(moderator, entry)))
      .toEqual([ false, false, true, true, false ])
    expect(ladder.map((entry) => isMovable(administrator, entry)))
      .toEqual([ false, true, true, true, false ])
  })
})

describe('moveRole', () => {
  it('swaps roles below the viewer', () => {
    expect(ids(moveRole(ladder, moderator, 'editor', 2)))
      .toEqual([ 'admin', 'moderator', 'editor', 'billing', 'default' ])
  })

  it('refuses a move that would lift a role over the viewer', () => {
    expect(moveRole(ladder, moderator, 'billing', 1)).toBeNull()
    expect(moveRole(ladder, moderator, 'moderator', 3)).toBeNull()
  })

  it('never moves a pinned role or past one', () => {
    expect(moveRole(ladder, administrator, 'editor', 4)).toBeNull()
    expect(moveRole(ladder, administrator, 'moderator', 0)).toBeNull()
    expect(moveRole(ladder, administrator, 'default', 3)).toBeNull()
  })

  it('answers nothing for a move that goes nowhere', () => {
    expect(moveRole(ladder, administrator, 'editor', 3)).toBeNull()
    expect(moveRole(ladder, administrator, 'ghost', 1)).toBeNull()
  })
})

describe('reorderRequest', () => {
  it('names every role but the pinned two, highest first', () => {
    expect(reorderRequest(ladder)).toEqual([ 'moderator', 'billing', 'editor' ])
  })
})
