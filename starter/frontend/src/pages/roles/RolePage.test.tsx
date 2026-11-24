// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it } from 'vitest'
import { screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const EDITOR_ID = '3f1a2b3c-0000-4000-8000-0000000000e1'
const MODERATOR_ID = '3f1a2b3c-0000-4000-8000-0000000000e2'

/** The signed-in account every case here renders as. */
const USER = {
  id: '3f1a2b3c-0000-4000-8000-0000000000ff',
  email: 'moderator@example.com',
  email_verified: true,
  first_name: null,
  last_name: null,
  time_zone: 'UTC',
  locale: 'en-US',
  created_at: '2026-01-01T00:00:00Z',
}

/** The viewer moderates: they manage roles and read projects, and nothing else. */
const MODERATOR = {
  roles: [{ id: MODERATOR_ID, name: 'Moderator', color: '#dc2626', position: 2 }],
  permissions: [ 'Tenancy::Role.update', 'Project.read' ],
  rank: 2,
  is_administrator: false,
}

/** The viewer administers the organization: they hold every permission. */
const ADMINISTRATOR = {
  roles: [{ id: 'admin', name: 'Administrator', color: '#e11d48', position: 3 }],
  permissions: [ 'Tenancy::Role.update', 'Project.read', 'Project.update' ],
  rank: 3,
  is_administrator: true,
}

function memberships(viewer: typeof MODERATOR) {
  return {
    organizations: [
      {
        id: ORGANIZATION_ID,
        name: 'Acme',
        is_member: true,
        ...viewer,
        teams: [],
      },
    ],
  }
}

function wireRole(id: string, name: string, position: number, permissions: string[]) {
  return {
    id,
    key: null,
    name,
    color: '#2563eb',
    position,
    permissions,
    customized: true,
    is_default: false,
    is_administrator: false,
    grantable_at: [ 'organization', 'sub_tenant', 'team' ],
  }
}

function catalog(viewer: typeof MODERATOR, grantable: string[]) {
  return {
    roles: [
      { ...wireRole('admin', 'Administrator', 3, []), key: 'admin', is_administrator: true },
      wireRole(MODERATOR_ID, 'Moderator', 2, [ 'Tenancy::Role.update', 'Project.read' ]),
      wireRole(EDITOR_ID, 'Editor', 1, [ 'Project.read', 'Project.update' ]),
      { ...wireRole('default', 'Member', 0, []), key: 'default', is_default: true },
    ],
    permissions: [ 'Project.read', 'Project.update', 'Project.destroy', 'Tenancy::Role.update' ],
    grantable,
    viewer,
  }
}

function stubBackend(
  viewer = MODERATOR,
  grantable = [ 'Project.read', 'Project.update', 'Project.destroy', 'Tenancy::Role.update' ],
) {
  stubFetch({
    '/auth/me': { status: 200, body: { user: USER }},
    '/tenancy/memberships': { status: 200, body: memberships(viewer) },
    '/account/notifications': { status: 200, body: { notifications: [], unread_count: 0 }},
    [`/tenancy/organizations/${ORGANIZATION_ID}/roles`]: {
      status: 200,
      body: catalog(viewer, grantable),
    },
  })
}

describe('RolePage', () => {
  it('locks the switches for permissions the viewer does not hold', async () => {
    stubBackend()
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/roles/${EDITOR_ID}`)

    const held = await screen.findByRole('switch', { name: 'Project: Read' })
    const notHeld = screen.getByRole('switch', { name: 'Project: Update' })

    expect(held).toHaveProperty('disabled', false)
    expect(notHeld, 'switching off what you lack is refused as well').toHaveProperty('disabled', true)
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', true)
  })

  it('reads a role at the viewer\'s own rank without offering to change it', async () => {
    stubBackend()
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/roles/${MODERATOR_ID}`)

    const toggle = await screen.findByRole('switch', { name: 'Project: Read' })
    expect(toggle).toHaveProperty('disabled', true)
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Delete role' })).toBeNull()
  })

  it('locks, even for an administrator, a switch no role in the application grants', async () => {
    stubBackend(ADMINISTRATOR, [ 'Project.read', 'Project.update', 'Tenancy::Role.update' ])
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/roles/${EDITOR_ID}`)

    const grantable = await screen.findByRole('switch', { name: 'Project: Update' })
    const ungrantable = screen.getByRole('switch', { name: 'Project: Delete' })

    expect(grantable).toHaveProperty('disabled', false)
    expect(ungrantable, 'the server would refuse it, so the screen does not offer it')
      .toHaveProperty('disabled', true)
  })
})
