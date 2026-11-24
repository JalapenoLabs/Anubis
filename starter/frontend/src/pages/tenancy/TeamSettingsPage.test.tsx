// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it } from 'vitest'
import { screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const TEAM_ID = '3f1a2b3c-0000-4000-8000-0000000000a1'
const EDITOR_ID = '3f1a2b3c-0000-4000-8000-0000000000e1'
const MANAGER_ID = '3f1a2b3c-0000-4000-8000-0000000000e2'

/** The signed-in account every case here renders as. */
const USER = {
  id: '3f1a2b3c-0000-4000-8000-0000000000ff',
  email: 'manager@example.com',
  email_verified: true,
  first_name: null,
  last_name: null,
  time_zone: 'UTC',
  locale: 'en-US',
  created_at: '2026-01-01T00:00:00Z',
}

/** Organization-wide, the viewer manages members: they staff the teams. */
const MANAGER = {
  roles: [{ id: MANAGER_ID, name: 'Manager', color: '#dc2626', position: 2 }],
  permissions: [ 'Tenancy::Membership.create' ],
  rank: 2,
  is_administrator: false,
}

/** In this team, the viewer holds only the role everybody holds. */
const EVERYBODY = {
  roles: [],
  permissions: [],
  rank: 0,
  is_administrator: false,
}

function wireRole(id: string, name: string, position: number) {
  return {
    id,
    key: null,
    name,
    color: '#2563eb',
    position,
    permissions: [],
    customized: true,
    is_default: false,
    is_administrator: false,
    grantable_at: [ 'organization', 'sub_tenant', 'team' ],
  }
}

function stubBackend() {
  stubFetch({
    '/auth/me': { status: 200, body: { user: USER }},
    '/tenancy/memberships': {
      status: 200,
      body: {
        organizations: [
          {
            id: ORGANIZATION_ID,
            name: 'Acme',
            is_member: true,
            ...MANAGER,
            teams: [{ id: TEAM_ID, name: 'Crew', ...EVERYBODY }],
          },
        ],
      },
    },
    '/account/notifications': { status: 200, body: { notifications: [], unread_count: 0 }},
    [`/tenancy/teams/${TEAM_ID}/members`]: { status: 200, body: { members: []}},
    [`/tenancy/teams/${TEAM_ID}/roles`]: {
      status: 200,
      body: {
        roles: [
          { ...wireRole('admin', 'Administrator', 3), key: 'admin', is_administrator: true },
          wireRole(MANAGER_ID, 'Manager', 2),
          wireRole(EDITOR_ID, 'Editor', 1),
          { ...wireRole('default', 'Member', 0), key: 'default', is_default: true },
        ],
        permissions: [ 'Tenancy::Membership.create' ],
        viewer: EVERYBODY,
      },
    },
  })
}

describe('TeamSettingsPage', () => {
  it('offers an organization manager the roles they may staff the team with', async () => {
    stubBackend()
    const { container } = renderWithProviders(<App />, `/teams/${TEAM_ID}/settings`)

    await screen.findAllByText('Crew', { exact: false })
    await waitFor(() => {
      const offered = Array.from(container.querySelectorAll('select option'))
        .map((option) => option.textContent)
      expect(offered, 'the manager outranks Editor organization-wide').toContain('Editor')
      expect(offered, 'and does not outrank their own role').not.toContain('Manager')
    })
  })
})
