// Copyright © 2026 Jalapeno Labs

// Core
import { useLocation } from 'react-router'

// UI
import { App } from './App'

// Utility
import { describe, expect, it } from 'vitest'
import { screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from './testing/harness'

/** Renders the router's current location, so a redirect can be asserted on. */
function LocationProbe() {
  const location = useLocation()

  return <output data-testid='location'>{
      `${location.pathname}${location.search}`
    }</output>
}

describe('RequireAuth', () => {
  it('should send a signed-out visitor to sign-in, carrying where they were headed', async () => {
    // 401 from `/auth/me` is the signed-out state, not an error.
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/oauth/providers': {
        status: 200,
        body: { providers: []},
      },
    })

    renderWithProviders(
      <>
        <LocationProbe />
        <App />
      </>,
      '/creative-concepts?page=2',
    )

    await waitFor(() => {
      expect(screen.getByTestId('location').textContent)
        .toBe('/sign-in?next=%2Fcreative-concepts%3Fpage%3D2')
    })

    // The sign-in page itself renders, rather than the guarded page.
    expect(screen.getByRole('heading', { name: 'Welcome back' })).toBeDefined()
  })
})
