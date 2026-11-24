// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const TEAM_A = '3f1a2b3c-0000-4000-8000-0000000000a1'
const TEAM_C = '3f1a2b3c-0000-4000-8000-0000000000c1'
const RACK = '3f1a2b3c-0000-4000-8000-0000000000d1'

/** The signed-in account every case here renders as. */
const USER = {
  id: '3f1a2b3c-0000-4000-8000-0000000000ff',
  email: 'steward@example.com',
  email_verified: true,
  first_name: null,
  last_name: null,
  time_zone: 'UTC',
  locale: 'en-US',
  created_at: '2026-01-01T00:00:00Z',
}

const MEMBERSHIPS = {
  organizations: [
    {
      id: ORGANIZATION_ID,
      name: 'Acme',
      is_member: true,
      roles: [],
      permissions: [],
      rank: 0,
      is_administrator: false,
      teams: [],
    },
  ],
}

type Standing = {
  reaches: boolean
  may_manage: boolean
}

function stubBackend(standing: Standing) {
  const grantsPath = `/tenancy/organizations/${ORGANIZATION_ID}/resource-grants`
  stubFetch({
    '/auth/me': { status: 200, body: { user: USER }},
    '/tenancy/memberships': { status: 200, body: MEMBERSHIPS },
    '/account/notifications': { status: 200, body: { notifications: [], unread_count: 0 }},
    [grantsPath]: {
      status: 200,
      body: {
        kinds: [{ kind: 'Machine', ...standing }],
        teams: standing.reaches
          ? [{ id: TEAM_A, name: 'Team A' }, { id: TEAM_C, name: 'Team C' }]
          : [],
        grants: standing.reaches
          ? [
              { team_id: TEAM_A, kind: 'Machine', every: true, resources: []},
              {
                team_id: TEAM_C,
                kind: 'Machine',
                every: false,
                resources: [{ value: RACK, label: 'Rack' }],
              },
            ]
          : [],
      },
    },
    [`${grantsPath}/Machine/options`]: {
      status: 200,
      body: { options: [{ value: RACK, label: 'Rack' }]},
    },
    [`/tenancy/organizations/${ORGANIZATION_ID}/teams/${TEAM_C}/resource-grants/Machine`]: {
      status: 200,
      body: { grant: { team_id: TEAM_C, kind: 'Machine', every: true, resources: []}},
    },
  })

  // The table above answers by path alone, so the writes are recorded here to
  // be read back by what they asked for.
  const answer = globalThis.fetch
  const writes: { path: string, body: unknown }[] = []
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    if (input instanceof Request && input.method === 'PUT') {
      writes.push({
        path: new URL(input.url).pathname,
        body: await input.clone().json(),
      })
    }
    return answer(input)
  })
  return writes
}

describe('ResourceAccessPage', () => {
  it('shows what each team reaches, and saves only the team that changed', async () => {
    const user = userEvent.setup()
    const writes = stubBackend({ reaches: true, may_manage: true })
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    const teamA = await screen.findByRole('radiogroup', { name: 'Team A: Machine' })
    const teamC = screen.getByRole('radiogroup', { name: 'Team C: Machine' })
    expect(within(teamA).getByRole('radio', { name: 'Every record' })).toHaveProperty('checked', true)
    expect(within(teamC).getByRole('radio', { name: 'Only these' })).toHaveProperty('checked', true)
    expect(screen.getByText('Rack')).toBeTruthy()

    const save = screen.getByRole('button', { name: 'Save' })
    expect(save, 'nothing to save before anything changed').toHaveProperty('disabled', true)

    await user.click(within(teamC).getByRole('radio', { name: 'Every record' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(writes).toHaveLength(1))
    expect(writes[0]).toEqual({
      path: `/tenancy/organizations/${ORGANIZATION_ID}/teams/${TEAM_C}/resource-grants/Machine`,
      body: { every: true, resource_ids: []},
    })
  })

  it('locks a kind the viewer may not share, and says why', async () => {
    stubBackend({ reaches: true, may_manage: false })
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    const teamA = await screen.findByRole('radiogroup', { name: 'Team A: Machine' })
    expect(within(teamA).getByRole('radio', { name: 'Nothing' })).toHaveProperty('disabled', true)
    expect(screen.getAllByText(
      'Sharing these takes the team access permission and every permission on them.',
    )).toHaveLength(2)
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull()
  })

  it('lists no grants of a kind the viewer does not reach', async () => {
    stubBackend({ reaches: false, may_manage: false })
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    expect(await screen.findByText('Sharing these takes roles that reach every one of them.')).toBeTruthy()
    expect(screen.queryByRole('radiogroup')).toBeNull()
  })
})
