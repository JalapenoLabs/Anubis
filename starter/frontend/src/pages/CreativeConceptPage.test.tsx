// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../App'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor, within } from '@testing-library/react'

// Misc
import { renderWithProviders } from '../testing/harness'
import { getCreativeConceptUrl } from '../urls'

const ORGANIZATION_ID = '5c1a2b3c-0000-4000-8000-000000000001'
const TEAM_ID = '5c1a2b3c-0000-4000-8000-0000000000aa'
const CONCEPT_ID = '5c1a2b3c-0000-4000-8000-0000000000bb'
const THING_ID = '5c1a2b3c-0000-4000-8000-0000000000cc'

const CONCEPT_PATH = `/account/creative-concepts/${CONCEPT_ID}`
const THINGS_PATH = `${CONCEPT_PATH}/tangible-things`

/** The signed-in account, an admin so the create form renders. */
const USER = {
  id: '5c1a2b3c-0000-4000-8000-0000000000ff',
  email: 'owner@example.com',
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
      roles: [ 'admin' ],
      teams: [
        {
          id: TEAM_ID,
          name: 'Acme HQ',
          roles: [ 'admin' ],
        },
      ],
    },
  ],
}

const CONCEPT = {
  id: CONCEPT_ID,
  team_id: TEAM_ID,
  name: 'Weather balloon',
  description: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

const CREATED_THING = {
  id: THING_ID,
  creative_concept_id: CONCEPT_ID,
  name: 'Altimeter',
  description: null,
  created_at: '2026-01-02T00:00:00Z',
  updated_at: '2026-01-02T00:00:00Z',
}

function pageOf(tangibleThings: unknown[]) {
  return {
    tangible_things: tangibleThings,
    pagination: {
      page: 1,
      limit: 10,
      total_items: tangibleThings.length,
      total_pages: tangibleThings.length ? 1 : 0,
    },
  }
}

function jsonResponse(status: number, body: unknown) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

/**
 * A backend whose first list read is held open until the test releases it.
 *
 * That read is the one the section starts as it mounts, and Playwright fills
 * and submits the create form faster than it can answer, which a person never
 * does. Holding it here reproduces that ordering deterministically, so what
 * the section does with a read that lands after the write is pinned rather
 * than left to how quickly a machine happens to answer.
 */
function stubBackendWithHeldFirstRead() {
  let releaseFirstRead = () => {}
  const firstReadHeld = new Promise<void>((resolve) => {
    releaseFirstRead = resolve
  })
  let listReads = 0

  vi.stubGlobal('fetch', async (input: Request | string | URL, init?: RequestInit) => {
    const requested = input instanceof Request
      ? input.url
      : String(input)
    const { pathname } = new URL(requested, 'http://localhost')
    const method = input instanceof Request
      ? input.method
      : init?.method ?? 'GET'

    if (pathname === '/auth/me') {
      return jsonResponse(200, { user: USER })
    }
    if (pathname === '/tenancy/memberships') {
      return jsonResponse(200, MEMBERSHIPS)
    }
    if (pathname === '/account/notifications') {
      return jsonResponse(200, { notifications: [], unread_count: 0 })
    }
    if (pathname === CONCEPT_PATH) {
      return jsonResponse(200, { creative_concept: CONCEPT })
    }
    if (pathname === THINGS_PATH && method === 'POST') {
      return jsonResponse(201, { tangible_thing: CREATED_THING })
    }
    if (pathname === THINGS_PATH) {
      listReads += 1
      if (listReads === 1) {
        await firstReadHeld
        // Answered from before the write, which is what makes it stale.
        return jsonResponse(200, pageOf([]))
      }
      return jsonResponse(200, pageOf([ CREATED_THING ]))
    }

    console.debug('the creative concept page asked for an unstubbed path', pathname)
    return jsonResponse(404, {})
  })

  return { releaseFirstRead }
}

describe('CreativeConceptPage', () => {
  it('should show a created tangible thing while the first list read is still in flight', async () => {
    const { releaseFirstRead } = stubBackendWithHeldFirstRead()

    renderWithProviders(<App />, getCreativeConceptUrl(CONCEPT_ID))

    // The page carries two forms with a field labelled "Name", so the create
    // form is addressed by the button that says what it is for.
    const createAction = await screen.findByRole('button', { name: 'Create tangible thing' })
    const createForm = createAction.closest('form')
    if (!createForm) {
      throw new Error('the create action is not inside a form, so this test cannot submit one')
    }

    const nameField = within(createForm).getByLabelText('Name')
    fireEvent.change(nameField, { target: { value: CREATED_THING.name }})
    fireEvent.submit(createForm)

    // The write revalidates the list itself rather than waiting for whatever
    // revalidation happens next, so the row is there without the held read.
    expect(await screen.findByText(CREATED_THING.name)).toBeTruthy()

    // And the read that was in flight when the write landed is discarded, not
    // rendered over the result.
    releaseFirstRead()
    await waitFor(() => expect(screen.getByText(CREATED_THING.name)).toBeTruthy())
    expect(screen.queryByText('No tangible things yet. Add the first one below.')).toBeNull()
  })
})
