// Copyright © 2026 Jalapeno Labs

import type { BillingWallet } from '@jalapenolabs/anubis'

// UI
import { AutoRechargeModal } from './AutoRechargeModal'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const AUTO_RECHARGE_PATH = `/billing/organizations/${ORGANIZATION_ID}/wallet/auto-recharge`

/** A usd wallet with a card a top-up saved and auto-recharge still off. */
const WALLET: BillingWallet = {
  currency: 'usd',
  balanceMicros: 25_000_000,
  minimumTopUpMicros: 10_000_000,
  lowBalanceMicros: 2_000_000,
  autoRecharge: null,
  cardSaved: true,
  recharging: false,
  rechargeFailedAt: null,
}

/** The same wallet as the API answers it, which both writes respond with. */
const WIRE_WALLET = {
  currency: 'usd',
  balance_micros: 25_000_000,
  minimum_top_up_micros: 10_000_000,
  low_balance_micros: 2_000_000,
  auto_recharge: null,
  card_saved: true,
  recharging: false,
  recharge_failed_at: null,
}

type Recorded = {
  method: string
  pathname: string
  body: unknown
}

/** Answers every request with `status` and `body`, recording what was sent. */
function stubRecording(status: number, body: unknown) {
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

    return Response.json(body, { status })
  })

  return sent
}

function renderModal(wallet: BillingWallet) {
  const onClose = vi.fn()
  const onChanged = vi.fn(async () => undefined)
  renderWithProviders(
    <AutoRechargeModal
      organizationId={ORGANIZATION_ID}
      wallet={wallet}
      isOpen
      onClose={onClose}
      onChanged={onChanged}
    />,
  )

  return {
    onClose,
    onChanged,
    threshold: screen.getByLabelText('Balance threshold'),
    amount: screen.getByLabelText('Recharge amount'),
    save: screen.getByRole('button', { name: 'Save' }),
  }
}

describe('AutoRechargeModal', () => {
  it('should show the form disabled, with the way to save a card, until one is saved', () => {
    const { threshold, amount, save } = renderModal({ ...WALLET, cardSaved: false })

    expect(screen.getByText(/Top up once with Save this card on/)).toBeDefined()
    expect(threshold.hasAttribute('disabled')).toBe(true)
    expect(amount.hasAttribute('disabled')).toBe(true)
    expect(save.hasAttribute('disabled')).toBe(true)
  })

  it('should suggest the smallest top-up below the low-balance line, with the cursor in it', async () => {
    const { threshold, amount } = renderModal(WALLET)

    await waitFor(() => {
      expect(threshold).toHaveProperty('value', '2')
    })
    expect(amount).toHaveProperty('value', '10')
    expect(document.activeElement).toBe(threshold)
    // Off already, so there is nothing to turn off.
    expect(screen.queryByRole('button', { name: 'Turn off' })).toBeNull()
  })

  it('should only offer Save for settings the server takes', async () => {
    const { threshold, amount, save } = renderModal(WALLET)
    await waitFor(() => {
      expect(save.hasAttribute('disabled')).toBe(false)
    })

    // Below the smallest top-up.
    fireEvent.change(amount, { target: { value: '5' }})
    expect(save.hasAttribute('disabled')).toBe(true)

    // Above the minimum, but one recharge would not clear the threshold.
    fireEvent.change(threshold, { target: { value: '15' }})
    fireEvent.change(amount, { target: { value: '12' }})
    expect(save.hasAttribute('disabled')).toBe(true)

    // A fraction of a cent.
    fireEvent.change(amount, { target: { value: '15.001' }})
    expect(save.hasAttribute('disabled')).toBe(true)

    // A threshold below zero.
    fireEvent.change(threshold, { target: { value: '-1' }})
    fireEvent.change(amount, { target: { value: '15' }})
    expect(save.hasAttribute('disabled')).toBe(true)

    fireEvent.change(threshold, { target: { value: '15' }})
    expect(save.hasAttribute('disabled')).toBe(false)
  })

  it('should put the settings, refresh the screen, and close', async () => {
    const sent = stubRecording(200, {
      ...WIRE_WALLET,
      auto_recharge: { threshold_micros: 5_000_000, amount_micros: 20_000_000 },
    })
    const { threshold, amount, save, onChanged, onClose } = renderModal(WALLET)

    fireEvent.change(threshold, { target: { value: '5' }})
    fireEvent.change(amount, { target: { value: '20' }})
    fireEvent.click(save)

    await waitFor(() => {
      expect(onClose).toHaveBeenCalled()
    })
    expect(onChanged).toHaveBeenCalled()
    expect(sent).toEqual([
      {
        method: 'PUT',
        pathname: AUTO_RECHARGE_PATH,
        body: { threshold_micros: 5_000_000, amount_micros: 20_000_000 },
      },
    ])
  })

  it('should show the server refusal on the form and stay open', async () => {
    stubRecording(409, {
      message: 'Auto-recharge charges a saved card. Top up once and save the card first.',
    })
    const { save, onChanged, onClose } = renderModal(WALLET)

    await waitFor(() => {
      expect(save.hasAttribute('disabled')).toBe(false)
    })
    fireEvent.click(save)

    expect(await screen.findByText(/Top up once and save the card first/)).toBeDefined()
    expect(onChanged).not.toHaveBeenCalled()
    expect(onClose).not.toHaveBeenCalled()
  })

  it('should turn auto-recharge off when it is on', async () => {
    const sent = stubRecording(200, WIRE_WALLET)
    const { threshold, amount, onChanged, onClose } = renderModal({
      ...WALLET,
      autoRecharge: { thresholdMicros: 5_000_000, amountMicros: 20_000_000 },
    })

    // Opening on the settings in force rather than on the suggestion.
    await waitFor(() => {
      expect(threshold).toHaveProperty('value', '5')
    })
    expect(amount).toHaveProperty('value', '20')

    fireEvent.click(screen.getByRole('button', { name: 'Turn off' }))

    await waitFor(() => {
      expect(onClose).toHaveBeenCalled()
    })
    expect(onChanged).toHaveBeenCalled()
    expect(sent).toEqual([
      { method: 'DELETE', pathname: AUTO_RECHARGE_PATH, body: null },
    ])
  })
})
