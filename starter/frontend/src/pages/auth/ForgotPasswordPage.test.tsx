// Copyright © 2026 Jalapeno Labs

// UI
import { ForgotPasswordPage } from './ForgotPasswordPage'

// Utility
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, screen } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

/** Lets every pending promise settle, and the fake clock run for `seconds`. */
async function settle(seconds: number = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(seconds * 1_000)
  })
}

describe('ForgotPasswordPage', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('should keep the form on screen and count a 429 down', async () => {
    vi.useFakeTimers()
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/password-reset/request': {
        status: 429,
        body: { message: 'Too many requests. Try again in 2 seconds.' },
        headers: { 'retry-after': '2' },
      },
    })

    const { container } = renderWithProviders(<ForgotPasswordPage />, '/forgot-password')
    await settle()

    fireEvent.change(screen.getByLabelText('Email'), {
      target: { value: 'someone@example.com' },
    })
    const submit = screen.getByRole('button', { name: 'Send reset link' })
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)
    await settle()

    // The form is still here: the address was fine, the timing was not.
    expect(screen.getByLabelText('Email')).toBeTruthy()
    expect(screen.getByText('Too many attempts. Try again in 2s')).toBeTruthy()
    expect(submit.hasAttribute('disabled')).toBe(true)

    await settle(2)
    expect(screen.queryByText(/Too many attempts/)).toBeNull()
    expect(submit.hasAttribute('disabled')).toBe(false)
  })

  it('should show what the backend says once a reset email is on its way', async () => {
    vi.useFakeTimers()
    stubFetch({
      '/auth/me': { status: 401 },
      '/auth/password-reset/request': {
        status: 202,
        body: { message: 'If that address has an account, a reset link is on its way.' },
      },
    })

    const { container } = renderWithProviders(<ForgotPasswordPage />, '/forgot-password')
    await settle()

    fireEvent.change(screen.getByLabelText('Email'), {
      target: { value: 'someone@example.com' },
    })
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)
    await settle()

    expect(
      screen.getByText('If that address has an account, a reset link is on its way.'),
    ).toBeTruthy()
    expect(screen.queryByText(/Too many attempts/)).toBeNull()
  })
})
