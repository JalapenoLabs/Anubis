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

const MEMBERSHIPS = {
  organizations: [
    {
      id: ORGANIZATION_ID,
      name: 'Acme',
      is_member: true,
      ...MODERATOR,
      teams: [],
    },
  ],
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
  }
}

const CATALOG = {
  roles: [
    { ...wireRole('admin', 'Administrator', 3, []), key: 'admin', is_administrator: true },
    wireRole(MODERATOR_ID, 'Moderator', 2, [ 'Tenancy::Role.update', 'Project.read' ]),
    wireRole(EDITOR_ID, 'Editor', 1, [ 'Project.read', 'Project.update' ]),
    { ...wireRole('default', 'Member', 0, []), key: 'default', is_default: true },
  ],
  permissions: [ 'Project.read', 'Project.update', 'Tenancy::Role.update' ],
  viewer: MODERATOR,
}

function stubBackend() {
  stubFetch({
    '/auth/me': { status: 200, body: { user: USER }},
    '/tenancy/memberships': { status: 200, body: MEMBERSHIPS },
    '/account/notifications': { status: 200, body: { notifications: [], unread_count: 0 }},
    [`/tenancy/organizations/${ORGANIZATION_ID}/roles`]: { status: 200, body: CATALOG },
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
})
