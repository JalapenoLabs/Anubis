// Copyright © 2026 Jalapeno Labs

// UI
import { SignInPage } from './SignInPage'

// Utility
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

/**
 * Lets every pending promise settle, and the fake clock run for `seconds`.
 *
 * Testing Library's own waiting helpers poll on timers it cannot advance under
 * Vitest's fake clock, so these cases drive the clock themselves.
 */
async function settle(seconds: number = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(seconds * 1_000)
  })
}

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

describe('SignInPage rate limiting', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('should count a 429 down and hold the submit until the wait is over', async () => {
    vi.useFakeTimers()
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/oauth/providers': { status: 200, body: { providers: []}},
      '/auth/login': {
        status: 429,
        body: { message: 'Too many requests. Try again in 3 seconds.' },
        headers: { 'retry-after': '3' },
      },
    })

    const { container } = renderWithProviders(<SignInPage />, '/sign-in')
    await settle()

    fireEvent.change(screen.getByLabelText('Email'), {
      target: { value: 'someone@example.com' },
    })
    fireEvent.change(screen.getByLabelText('Password'), {
      target: { value: 'correct horse battery staple' },
    })
    await settle()

    const submit = screen.getByRole('button', { name: 'Sign in' })
    expect(submit.hasAttribute('disabled')).toBe(false)

    const form = container.querySelector('form')
    expect(form).not.toBeNull()
    fireEvent.submit(form as HTMLFormElement)
    await settle()

    // The wait, not the backend's sentence: the number moves every second.
    expect(screen.getByText('Too many attempts. Try again in 3s')).toBeTruthy()
    expect(submit.hasAttribute('disabled')).toBe(true)

    await settle(1)
    expect(screen.getByText('Too many attempts. Try again in 2s')).toBeTruthy()
    expect(submit.hasAttribute('disabled')).toBe(true)

    await settle(2)
    expect(screen.queryByText(/Too many attempts/)).toBeNull()
    expect(submit.hasAttribute('disabled')).toBe(false)
  })

  it('should show the backend message when a refusal is not a rate limit', async () => {
    vi.useFakeTimers()
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/oauth/providers': { status: 200, body: { providers: []}},
      '/auth/login': {
        status: 401,
        body: { message: 'Those credentials do not match an account.' },
      },
    })

    const { container } = renderWithProviders(<SignInPage />, '/sign-in')
    await settle()

    fireEvent.change(screen.getByLabelText('Email'), {
      target: { value: 'someone@example.com' },
    })
    fireEvent.change(screen.getByLabelText('Password'), {
      target: { value: 'correct horse battery staple' },
    })
    await settle()

    fireEvent.submit(container.querySelector('form') as HTMLFormElement)
    await settle()

    expect(screen.getByText('Those credentials do not match an account.')).toBeTruthy()
    expect(screen.queryByText(/Too many attempts/)).toBeNull()
  })
})
