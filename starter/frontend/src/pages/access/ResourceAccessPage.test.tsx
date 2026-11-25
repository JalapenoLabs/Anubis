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

const TEAM_B = '3f1a2b3c-0000-4000-8000-0000000000b1'
const GRANTS_PATH = `/tenancy/organizations/${ORGANIZATION_ID}/resource-grants`

function overview(standing: Standing, teams = [{ id: TEAM_A, name: 'Team A' }, { id: TEAM_C, name: 'Team C' }]) {
  return {
    kinds: [{ kind: 'Machine', ...standing }],
    teams: standing.reaches
      ? teams
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
  }
}

/**
 * The backend these cases render against, with the listing and the writes
 * under the test's control: `listing` is what the next read of the grants
 * answers, and a path in `refusing` answers its write `403`.
 */
function stubBackend(standing: Standing) {
  const backend = {
    listing: overview(standing) as unknown,
    refusing: new Set<string>(),
    writes: [] as { path: string, body: unknown }[],
  }
  stubFetch({
    '/auth/me': { status: 200, body: { user: USER }},
    '/tenancy/memberships': { status: 200, body: MEMBERSHIPS },
    '/account/notifications': { status: 200, body: { notifications: [], unread_count: 0 }},
    [`${GRANTS_PATH}/Machine/options`]: {
      status: 200,
      body: { options: [{ value: RACK, label: 'Rack' }]},
    },
  })

  const answer = globalThis.fetch
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const request = input instanceof Request
      ? input
      : new Request(new URL(String(input), 'http://localhost'))
    const path = new URL(request.url).pathname
    if (request.method === 'PUT') {
      backend.writes.push({ path, body: await request.clone().json() })
      const status = backend.refusing.has(path)
        ? 403
        : 200
      const body = status === 200
        ? { grant: { team_id: '', kind: 'Machine', every: true, resources: []}}
        : { message: 'You do not have permission to do that.' }
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      })
    }
    if (path === GRANTS_PATH) {
      return new Response(JSON.stringify(backend.listing), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    }
    return answer(input)
  })
  return backend
}

function writePath(teamId: string) {
  return `/tenancy/organizations/${ORGANIZATION_ID}/teams/${teamId}/resource-grants/Machine`
}

describe('ResourceAccessPage', () => {
  it('shows what each team reaches, and saves only the team that changed', async () => {
    const user = userEvent.setup()
    const { writes } = stubBackend({ reaches: true, may_manage: true })
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
      path: writePath(TEAM_C),
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

  it('keeps working when the organization\'s teams change underneath it', async () => {
    const user = userEvent.setup()
    const backend = stubBackend({ reaches: true, may_manage: true })
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    const teamC = await screen.findByRole('radiogroup', { name: 'Team C: Machine' })
    await user.click(within(teamC).getByRole('radio', { name: 'Every record' }))

    // The save reads the grants back, and by then a team was added and one
    // went: the screen redraws from the new list rather than falling over.
    backend.listing = overview(
      { reaches: true, may_manage: true },
      [{ id: TEAM_B, name: 'Team B' }, { id: TEAM_C, name: 'Team C' }],
    )
    await user.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByRole('radiogroup', { name: 'Team B: Machine' })).toBeTruthy()
    expect(screen.queryByRole('radiogroup', { name: 'Team A: Machine' })).toBeNull()
  })

  it('keeps the edits a refused save did not reach, and says which team refused', async () => {
    const user = userEvent.setup()
    const backend = stubBackend({ reaches: true, may_manage: true })
    backend.refusing.add(writePath(TEAM_A))
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    const teamA = await screen.findByRole('radiogroup', { name: 'Team A: Machine' })
    const teamC = screen.getByRole('radiogroup', { name: 'Team C: Machine' })
    await user.click(within(teamA).getByRole('radio', { name: 'Nothing' }))
    await user.click(within(teamC).getByRole('radio', { name: 'Nothing' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))

    expect(await screen.findByText('Team A: You do not have permission to do that.')).toBeTruthy()
    expect(backend.writes.map((write) => write.path)).toEqual([ writePath(TEAM_A) ])
    expect(
      within(screen.getByRole('radiogroup', { name: 'Team C: Machine' }))
        .getByRole('radio', { name: 'Nothing' }),
      'an edit the save never reached is still there to save',
    ).toHaveProperty('checked', true)
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', false)
  })

  it('sends the organization back to its settings when it shares nothing', async () => {
    const backend = stubBackend({ reaches: true, may_manage: true })
    backend.listing = { kinds: [], teams: [], grants: []}
    renderWithProviders(<App />, `/organizations/${ORGANIZATION_ID}/access`)

    expect(await screen.findByRole('heading', { name: 'Organization settings' })).toBeTruthy()
    await waitFor(() => expect(screen.queryByText('Manage team access')).toBeNull())
  })
})
