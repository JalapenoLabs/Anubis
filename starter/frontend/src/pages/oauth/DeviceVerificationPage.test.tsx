// Copyright © 2026 Jalapeno Labs

// Core
import { useLocation } from 'react-router'

// UI
import { DeviceVerificationPage } from './DeviceVerificationPage'

// Utility
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

/** Renders the router's current location, so a navigation can be asserted on. */
function LocationProbe() {
  const location = useLocation()

  return <output data-testid='location'>{
      `${location.pathname}${location.search}`
    }</output>
}

/** Answers code entry with `answer`, recording what each entry sent. */
function stubEntry(entries: unknown[], answer: Response) {
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const request = input instanceof Request
      ? input
      : new Request(String(input))
    const { pathname } = new URL(request.url, 'http://localhost')

    if (pathname === '/oauth/device_verification' && request.method === 'POST') {
      entries.push(await request.json())
      return answer
    }
    return new Response('{}', { status: 404 })
  })
}

describe('DeviceVerificationPage', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('should take a typed code to the consent screen for the request it names', async () => {
    const entries: unknown[] = []
    stubEntry(entries, Response.json({ request_id: 'request-7' }))

    const { container } = renderWithProviders(
      <>
        <LocationProbe />
        <DeviceVerificationPage />
      </>,
      '/device',
    )

    const submit = screen.getByRole('button', { name: 'Continue' })
    expect(submit.hasAttribute('disabled')).toBe(true)
    // Typed the way people type it: lowercase, without the dash.
    fireEvent.change(screen.getByLabelText('Code'), { target: { value: 'wdjbmjht' }})
    expect(submit.hasAttribute('disabled')).toBe(false)
    expect(screen.queryByText(/Check that this matches/)).toBeNull()
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)

    await waitFor(() => expect(screen.getByTestId('location').textContent)
      .toBe('/consent?request=request-7'))
    expect(entries).toEqual([{ user_code: 'WDJBMJHT' }])
  })

  it('should prefill a linked code and ask for it to be checked, sending nothing yet', async () => {
    const entries: unknown[] = []
    stubEntry(entries, Response.json({ request_id: 'request-7' }))

    renderWithProviders(<DeviceVerificationPage />, '/device?user_code=WDJB-MJHT')

    expect(screen.getByLabelText<HTMLInputElement>('Code').value).toBe('WDJB-MJHT')
    expect(screen.getByText('Check that this matches the code your device shows.')).toBeTruthy()
    // A link anybody can send is never acted on until the person presses on.
    await act(async () => {
      await Promise.resolve()
    })
    expect(entries).toEqual([])
  })

  it('should say a refused code is not valid, whatever made it so', async () => {
    stubEntry([], Response.json({ message: 'That code is not valid.' }, { status: 400 }))

    const { container } = renderWithProviders(<DeviceVerificationPage />, '/device?user_code=WDJB-MJHT')
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)

    expect(await screen.findByText('That code is not valid. Check it against your device\'s screen.'))
      .toBeTruthy()
  })

  it('should send an account on a temporary password to choose its own', async () => {
    stubFetch({
      '/oauth/device_verification': {
        status: 403,
        body: {
          message: 'Choose a new password before you continue.',
          code: 'password_change_required',
        },
      },
    })

    const { container } = renderWithProviders(<DeviceVerificationPage />, '/device?user_code=WDJB-MJHT')
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)

    expect(await screen.findByText(/Your account is on a temporary password/)).toBeTruthy()
    const link = screen.getByRole('link', { name: 'Choose a new password' })
    expect(link.getAttribute('href')).toBe('/settings/security')
    expect(screen.queryByText('Choose a new password before you continue.')).toBeNull()
  })

  it('should count a rate-limited entry down rather than refuse the code', async () => {
    vi.useFakeTimers()
    stubFetch({
      '/oauth/device_verification': {
        status: 429,
        body: { message: 'Too many requests. Try again in 2 seconds.' },
        headers: { 'retry-after': '2' },
      },
    })

    const { container } = renderWithProviders(<DeviceVerificationPage />, '/device?user_code=WDJB-MJHT')
    fireEvent.submit(container.querySelector('form') as HTMLFormElement)
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })

    expect(screen.getByText('Too many attempts. Try again in 2s')).toBeTruthy()
    expect(screen.queryByText(/is not valid/)).toBeNull()
  })

  it('should say how the decision went when the backend returns the browser', () => {
    const { unmount } = renderWithProviders(<DeviceVerificationPage />, '/device?result=approved')
    expect(screen.getByText('Device approved')).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Connect another device' }).getAttribute('href'))
      .toBe('/device')
    unmount()

    renderWithProviders(<DeviceVerificationPage />, '/device?result=denied')
    expect(screen.getByText('Device denied')).toBeTruthy()
    expect(screen.queryByLabelText('Code')).toBeNull()
  })

  it('should treat a result it does not know as no result', () => {
    renderWithProviders(<DeviceVerificationPage />, '/device?result=elsewhere')

    expect(screen.getByLabelText('Code')).toBeTruthy()
  })
})
