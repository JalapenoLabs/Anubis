// Copyright © 2026 Jalapeno Labs

// UI
import { SignUpPage } from './SignUpPage'

// Utility
import { describe, expect, it } from 'vitest'
import { screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

describe('SignUpPage', () => {
  it('should render the form while the deployment accepts registrations', async () => {
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/registration': {
        status: 200,
        body: { open: true },
      },
    })

    renderWithProviders(<SignUpPage />, '/sign-up')

    await screen.findByRole('button', { name: 'Create account' })
  })

  it('should explain itself instead of rendering a form nobody may submit', async () => {
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/registration': {
        status: 200,
        body: { open: false },
      },
    })

    renderWithProviders(<SignUpPage />, '/sign-up')

    await screen.findByText('Registration is closed')
    expect(screen.queryByRole('button', { name: 'Create account' })).toBeNull()
    // The way in for somebody who already has an account.
    expect(screen.getByRole('link', { name: 'Sign in' })).toBeTruthy()
  })
})
