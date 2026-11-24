// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, within } from '@testing-library/react'

// Misc
import { renderWithProviders } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const ACTIVITY_PATH = `/organizations/${ORGANIZATION_ID}/billing/activity`
const ENTRIES_PATH = `/billing/organizations/${ORGANIZATION_ID}/wallet/entries`

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

function memberships(organizationRoles: string[]) {
  return {
    organizations: [
      {
        id: ORGANIZATION_ID,
        name: 'Acme',
        roles: organizationRoles,
        teams: [
          {
            id: '3f1a2b3c-0000-4000-8000-0000000000aa',
            name: 'Acme HQ',
            roles: [ 'admin' ],
          },
        ],
      },
    ],
  }
}

const WALLET = {
  currency: 'usd',
  balance_micros: -25_000_000,
  minimum_top_up_micros: 10_000_000,
  low_balance_micros: 2_000_000,
  auto_recharge: null,
  card_saved: true,
  recharging: false,
  recharge_failed_at: null,
}

function billing(wallet: unknown) {
  return {
    plan: {
      key: 'free',
      name: 'Free',
      description: null,
      highlighted: false,
      prices: {},
      limits: {},
    },
    plan_source: 'free',
    subscription: null,
    subscriptions: [],
    billing_enabled: true,
    seats_used: 1,
    wallet,
  }
}

/** One ledger line on the wire; noon UTC, so its date reads the same in any time zone. */
function entry(position: number, fields: Record<string, unknown>) {
  return {
    id: `3f1a2b3c-0000-4000-8000-00000000020${position}`,
    organization_id: ORGANIZATION_ID,
    position,
    requested_micros: null,
    idempotency_key: `entry-${position}`,
    stripe_payment_intent_id: null,
    description: null,
    subject_type: null,
    subject_id: null,
    created_at: `2026-11-2${position}T12:00:00Z`,
    ...fields,
  }
}

// The ledger oldest first: $25.00 bought, $0.0875 of usage, a spend that
// asked for $30.00 and drained what was left, then Stripe refunding the
// $25.00 payment, which takes the balance below zero.
const TOP_UP = entry(1, {
  kind: 'top_up',
  amount_micros: 25_000_000,
  balance_after_micros: 25_000_000,
  stripe_payment_intent_id: 'pi_test',
})
const SUB_CENT_SPEND = entry(2, {
  kind: 'spend',
  amount_micros: -87_500,
  balance_after_micros: 24_912_500,
  requested_micros: 87_500,
  description: 'One hour of server time',
})
const DRAINED_SPEND = entry(3, {
  kind: 'spend',
  amount_micros: -24_912_500,
  balance_after_micros: 0,
  requested_micros: 30_000_000,
  description: 'A month of storage',
})
const REFUND = entry(4, {
  kind: 'refund',
  amount_micros: -25_000_000,
  balance_after_micros: -25_000_000,
  stripe_payment_intent_id: 'pi_test',
})

/** Newest first, two to a page, as the endpoint pages them. */
const PAGES: Record<string, unknown[]> = {
  1: [ REFUND, DRAINED_SPEND ],
  2: [ SUB_CENT_SPEND, TOP_UP ],
}

/**
 * Answers the page's requests, paging the ledger by its `page` parameter.
 *
 * `stubFetch` keys on the path alone, and two pages of one path need the query
 * too, so this stub reads it the way `ConnectedClientsCard.test.tsx` answers a
 * list that changes. Every ledger request is recorded, so a case can prove one
 * was never sent.
 */
function stubLedger(organizationRoles: string[], wallet: unknown = WALLET) {
  const ledgerRequests: string[] = []
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const requested = input instanceof Request
      ? input.url
      : String(input)
    const url = new URL(requested, 'http://localhost')

    if (url.pathname === '/auth/me') {
      return Response.json({ user: USER })
    }
    if (url.pathname === '/tenancy/memberships') {
      return Response.json(memberships(organizationRoles))
    }
    if (url.pathname === `/billing/organizations/${ORGANIZATION_ID}`) {
      return Response.json(billing(wallet))
    }
    if (url.pathname === ENTRIES_PATH) {
      ledgerRequests.push(url.search)
      const page = url.searchParams.get('page') ?? '1'
      return Response.json({
        entries: PAGES[page] ?? [],
        pagination: { page: Number(page), limit: 2, total_items: 4, total_pages: 2 },
      })
    }

    console.debug('stubLedger has no response for', url.pathname)
    return Response.json({}, { status: 404 })
  })

  return ledgerRequests
}

/** The text of every body row, which is what a person reads off the table. */
function rowTexts() {
  const [ , ...rows ] = screen.getAllByRole('row')
  return rows.map((row) => row.textContent)
}

describe('CreditActivityPage', () => {
  it('should page through the ledger with every amount to the micro-unit', async () => {
    const ledgerRequests = stubLedger([ 'billing' ])

    renderWithProviders(<App />, ACTIVITY_PATH)

    expect(await screen.findByRole('heading', { name: 'Credit activity' })).toBeDefined()
    expect(await screen.findByText('Refunded through Stripe')).toBeDefined()

    // The refund took the balance below zero, and reads as a debit.
    const [ refund, drained ] = rowTexts()
    expect(refund).toContain('Refund')
    expect(refund).toContain('November 24, 2026')
    expect(refund).toContain('-$25.00')
    // A spend that drained the wallet says how much of it was covered.
    expect(drained).toContain('A month of storage')
    expect(drained).toContain('-$24.9125')
    expect(drained).toContain('Covered $24.9125 of $30.00')
    expect(drained).toContain('$0.00')

    fireEvent.click(screen.getByRole('button', { name: 'pagination item 2' }))

    expect(await screen.findByText('One hour of server time')).toBeDefined()
    const [ subCent, topUp ] = rowTexts()
    // A sub-cent charge is not rounded away, and needs no covered note.
    expect(subCent).toContain('-$0.0875')
    expect(subCent).toContain('$24.9125')
    expect(subCent).not.toContain('Covered')
    // A credit is signed, green, and described by its kind when the entry
    // carries no words of its own.
    expect(topUp).toContain('Top-up')
    expect(topUp).toContain('Credit bought at checkout')
    const topUpAmount = within(screen.getAllByRole('row')[2]).getByText('+$25.00')
    expect(topUpAmount.closest('td')?.className).toContain('text-success')

    expect(ledgerRequests).toEqual([ '?page=1&limit=25', '?page=2&limit=25' ])
  })

  it('should trail back to the billing screen it belongs under', async () => {
    stubLedger([ 'admin' ])

    renderWithProviders(<App />, ACTIVITY_PATH)

    const trail = await screen.findByRole('navigation', { name: 'Breadcrumb' })
    expect(within(trail).getByRole('link', { name: 'Billing' }).getAttribute('href'))
      .toBe(`/organizations/${ORGANIZATION_ID}/billing`)
    expect(within(trail).getByText('Activity')).toBeDefined()
  })

  it('should refuse a member without a billing role, as the endpoint does', async () => {
    const ledgerRequests = stubLedger([ 'editor' ])

    renderWithProviders(<App />, ACTIVITY_PATH)

    expect(await screen.findByText(/can read the credit activity/)).toBeDefined()
    expect(screen.queryByRole('table')).toBeNull()
    expect(ledgerRequests).toEqual([])
  })

  it('should go back to the billing screen for an application that sells no credit', async () => {
    const ledgerRequests = stubLedger([ 'admin' ], null)

    renderWithProviders(<App />, ACTIVITY_PATH)

    expect(await screen.findByText('Current plan: Free')).toBeDefined()
    expect(screen.queryByRole('heading', { name: 'Credit activity' })).toBeNull()
    expect(ledgerRequests).toEqual([])
  })
})
