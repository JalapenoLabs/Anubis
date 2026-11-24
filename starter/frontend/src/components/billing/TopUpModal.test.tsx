// Copyright © 2026 Jalapeno Labs

// UI
import { TopUpModal } from './TopUpModal'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'

/** The wallet `config/billing.yml` sells: usd, $10.00 at least, no card yet. */
const WALLET = {
  currency: 'usd',
  balanceMicros: 0,
  minimumTopUpMicros: 10_000_000,
  chargeStepMicros: 10_000,
  lowBalanceMicros: 2_000_000,
  autoRecharge: null,
  cardSaved: false,
  recharging: false,
  rechargeFailedAt: null,
}

type Recorded = {
  method: string
  pathname: string
  body: unknown
}

/**
 * Answers the top-up with a refusal and records what was sent.
 *
 * A refusal rather than a Stripe URL, so the test never sends the page away:
 * the request is what proves the form, and the refusal is what proves the
 * error lands form-level.
 */
function stubRefusedTopUp(message: string) {
  const sent: Recorded[] = []
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const request = input instanceof Request
      ? input
      : new Request(String(input))
    const { pathname } = new URL(request.url, 'http://localhost')
    const text = await request.text()
    sent.push({
      method: request.method,
      pathname,
      body: text
        ? JSON.parse(text)
        : null,
    })

    return Response.json({ message }, { status: 400 })
  })

  return sent
}

function renderModal(onClose = vi.fn()) {
  renderWithProviders(
    <TopUpModal
      organizationId={ORGANIZATION_ID}
      wallet={WALLET}
      isOpen
      onClose={onClose}
    />,
  )

  return {
    amount: screen.getByLabelText('Amount'),
    submit: screen.getByRole('button', { name: 'Continue to Stripe' }),
  }
}

describe('TopUpModal', () => {
  it('should open on the minimum, with the cursor in the amount', async () => {
    const { amount } = renderModal()

    await waitFor(() => {
      expect(amount).toHaveProperty('value', '10')
    })
    expect(document.activeElement).toBe(amount)
  })

  it('should hold Continue back until the amount is one Stripe can charge', async () => {
    const { amount, submit } = renderModal()
    await waitFor(() => {
      expect(submit.hasAttribute('disabled')).toBe(false)
    })

    fireEvent.change(amount, { target: { value: '5' }})
    expect(submit.hasAttribute('disabled')).toBe(true)

    // A fraction of a cent is the server's 400, so it is refused here first.
    fireEvent.change(amount, { target: { value: '10.005' }})
    expect(submit.hasAttribute('disabled')).toBe(true)

    fireEvent.change(amount, { target: { value: '' }})
    expect(submit.hasAttribute('disabled')).toBe(true)

    fireEvent.change(amount, { target: { value: '25.50' }})
    expect(submit.hasAttribute('disabled')).toBe(false)
  })

  it('should send the amount and the card choice, and show a refusal on the form', async () => {
    const sent = stubRefusedTopUp('Stripe cannot charge this amount.')
    const { amount, submit } = renderModal()

    fireEvent.change(amount, { target: { value: '25' }})
    // The label is part of the switch, so clicking the words turns it on.
    fireEvent.click(screen.getByText('Save this card for auto-recharge'))
    fireEvent.click(submit)

    expect(await screen.findByText('Stripe cannot charge this amount.')).toBeDefined()
    expect(sent).toEqual([
      {
        method: 'POST',
        pathname: `/billing/organizations/${ORGANIZATION_ID}/wallet/top-up`,
        body: { amount_micros: 25_000_000, save_card: true },
      },
    ])
  })

  it('should close on Cancel without sending anything', () => {
    const sent = stubRefusedTopUp('unused')
    const onClose = vi.fn()
    renderModal(onClose)

    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))

    expect(onClose).toHaveBeenCalled()
    expect(sent).toEqual([])
  })
})
