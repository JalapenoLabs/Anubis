// Copyright © 2026 Jalapeno Labs

// UI
import { SignInPage } from './SignInPage'

// Utility
import { describe, expect, it } from 'vitest'
import { screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

describe('SignInPage', () => {
  it('should render one provider button per configured provider, keeping the destination', async () => {
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/oauth/providers': {
        status: 200,
        body: {
          providers: [
            {
              key: 'google',
              display_name: 'Google',
            },
            {
              key: 'entra',
              display_name: 'Microsoft Entra ID',
            },
          ],
        },
      },
    })

    renderWithProviders(<SignInPage />, '/sign-in?next=%2Fcreative-concepts')

    // The buttons are links the browser follows out of the SPA, which HeroUI
    // renders as an anchor carrying the button role.
    const google = await screen.findByRole('button', { name: 'Continue with Google' })
    expect(google.getAttribute('href'))
      .toBe('/auth/oauth/google/start?next=%2Fcreative-concepts')

    // The page renders whatever the backend reports, including a provider no
    // scaffold ever wrote a button for.
    const entra = screen.getByRole('button', { name: 'Continue with Microsoft Entra ID' })
    expect(entra.getAttribute('href'))
      .toBe('/auth/oauth/entra/start?next=%2Fcreative-concepts')

    expect(screen.getAllByRole('button', { name: /^Continue with/ })).toHaveLength(2)
  })
})
