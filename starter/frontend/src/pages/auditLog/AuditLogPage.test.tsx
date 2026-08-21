// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it } from 'vitest'
import { fireEvent, screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const TEAM_ID = '3f1a2b3c-0000-4000-8000-0000000000aa'
const AUDIT_PATH = `/teams/${TEAM_ID}/audit-log`

/** The signed-in account every case here renders as. */
const USER = {
  id: '3f1a2b3c-0000-4000-8000-0000000000ff',
  email: 'owner@example.com',
  email_verified: true,
  first_name: null,
  last_name: null,
  time_zone: 'UTC',
  locale: 'en-US',
  created_at: '2026-01-01T00:00:00Z',
}

function memberships(teamRoles: string[]) {
  return {
    organizations: [
      {
        id: ORGANIZATION_ID,
        name: 'Acme',
        roles: [ 'admin' ],
        teams: [
          {
            id: TEAM_ID,
            name: 'Acme HQ',
            roles: teamRoles,
          },
        ],
      },
    ],
  }
}

/** A rename, which is the smallest act with both a before and an after. */
const RENAMED = {
  id: '3f1a2b3c-0000-4000-8000-000000000101',
  team_id: TEAM_ID,
  organization_id: null,
  user_id: USER.id,
  actor_name: 'Ada Lovelace',
  action: 'team.renamed',
  subject_type: 'Team',
  subject_id: TEAM_ID,
  subject_label: 'Acme HQ',
  changes: {
    name: { old: 'General', new: 'Acme HQ' },
  },
  request_id: 'req-1',
  created_at: new Date().toISOString(),
}

/** A framework act nobody performed, which the screen names as the system. */
const SYSTEM_ACT = {
  ...RENAMED,
  id: '3f1a2b3c-0000-4000-8000-000000000102',
  user_id: null,
  actor_name: null,
  action: 'member.removed',
  subject_type: 'TeamMembership',
  subject_label: 'guest@example.com',
  changes: {},
  request_id: null,
}

function stubAuditLog(events: unknown[], teamRoles: string[] = [ 'admin' ]) {
  stubFetch({
    '/auth/me': {
      status: 200,
      body: { user: USER },
    },
    '/tenancy/memberships': {
      status: 200,
      body: memberships(teamRoles),
    },
    [`/account/teams/${TEAM_ID}/audit-events`]: {
      status: 200,
      body: {
        audit_events: events,
        pagination: {
          page: 1,
          limit: 25,
          total_items: events.length,
          total_pages: 1,
        },
      },
    },
  })
}

describe('AuditLogPage', () => {
  it('should render the actor, the action, the subject, and when it happened', async () => {
    stubAuditLog([ RENAMED ])

    renderWithProviders(<App />, AUDIT_PATH)

    expect(await screen.findByText('Ada Lovelace')).toBeDefined()

    // Asserted on the row rather than the page: "Acme HQ" is also the team in
    // the switcher, and "Team" sits under the label as the subject type.
    const [ , row ] = screen.getAllByRole('row')
    expect(row.textContent).toContain('Ada Lovelace')
    expect(row.textContent).toContain('team.renamed')
    expect(row.textContent).toContain('Acme HQ')
    expect(row.textContent).toContain('Team')
    // The row was written this instant, so it reads as the present.
    expect(row.textContent).toContain('now')
  })

  it('should unfold the change set only when asked', async () => {
    stubAuditLog([ RENAMED ])

    renderWithProviders(<App />, AUDIT_PATH)

    const toggle = await screen.findByRole('button', { name: '1 field' })
    expect(screen.queryByText('"General"')).toBeNull()

    fireEvent.click(toggle)

    expect(screen.getByText('"General"')).toBeDefined()
    expect(screen.getByText('"Acme HQ"')).toBeDefined()
  })

  it('should name the system when an act had no actor, and say when nothing moved', async () => {
    stubAuditLog([ SYSTEM_ACT ])

    renderWithProviders(<App />, AUDIT_PATH)

    expect(await screen.findByText('System')).toBeDefined()
    expect(screen.getByText('No fields changed')).toBeDefined()
  })

  it('should say the log is empty rather than showing a bare table', async () => {
    stubAuditLog([])

    renderWithProviders(<App />, AUDIT_PATH)

    expect(await screen.findByText('Nothing has been recorded for this team yet.')).toBeDefined()
  })

  it('should refuse a member without the admin role, as the endpoint does', async () => {
    stubAuditLog([ RENAMED ], [ 'editor' ])

    renderWithProviders(<App />, AUDIT_PATH)

    expect(await screen.findByText('Only a team admin can read the audit log.')).toBeDefined()
    expect(screen.queryByText('team.renamed')).toBeNull()
  })
})
