// Copyright © 2026 Jalapeno Labs

import type { MembershipOrganization } from '@jalapenolabs/anubis'

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { findTeam, resolveCurrentTeam } from './TeamProvider'

const organizations: MembershipOrganization[] = [
  {
    id: 'organization-acme',
    name: 'Acme',
    roles: [ 'admin' ],
    teams: [
      {
        id: 'team-general',
        name: 'General',
        roles: [ 'admin' ],
      },
      {
        id: 'team-platform',
        name: 'Platform',
        roles: [ 'editor' ],
      },
    ],
  },
  {
    id: 'organization-side',
    name: 'Side Project',
    roles: [],
    teams: [
      {
        id: 'team-side',
        name: 'General',
        roles: [ 'default' ],
      },
    ],
  },
]

describe('findTeam', () => {
  it('should return the team and the organization it belongs to', () => {
    expect(findTeam(organizations, 'team-side')).toEqual({
      organization: organizations[1],
      team: organizations[1].teams[0],
    })
  })

  it('should return null for a team the user does not belong to', () => {
    expect(findTeam(organizations, 'team-elsewhere')).toBeNull()
  })

  it('should return null when the overview is still empty', () => {
    expect(findTeam([], 'team-general')).toBeNull()
  })
})

describe('resolveCurrentTeam', () => {
  it('should return the selected team', () => {
    expect(resolveCurrentTeam(organizations, 'team-platform')).toEqual({
      organization: organizations[0],
      team: organizations[0].teams[1],
    })
  })

  it('should fall back to the first team when the selection is gone', () => {
    // What a deleted team, or one the user left, leaves behind.
    expect(resolveCurrentTeam(organizations, 'team-deleted')).toEqual({
      organization: organizations[0],
      team: organizations[0].teams[0],
    })
  })

  it('should fall back to the first team when nothing is selected', () => {
    expect(resolveCurrentTeam(organizations, null)).toEqual({
      organization: organizations[0],
      team: organizations[0].teams[0],
    })
  })

  it('should return null when the user belongs to no team at all', () => {
    expect(resolveCurrentTeam([], 'team-general')).toBeNull()
  })
})
