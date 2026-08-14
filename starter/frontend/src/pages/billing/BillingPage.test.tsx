// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it } from 'vitest'
import { screen, waitFor } from '@testing-library/react'

// Misc
import { renderWithProviders, stubFetch } from '../../testing/harness'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'
const BILLING_PATH = `/organizations/${ORGANIZATION_ID}/billing`

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

/** One organization the account administers, with one team in it. */
const MEMBERSHIPS = {
  organizations: [
    {
      id: ORGANIZATION_ID,
      name: 'Acme',
      roles: [ 'admin' ],
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

/** The free plan as `config/billing.yml` defines it, on the wire. */
const FREE_PLAN = {
  key: 'free',
  name: 'Free',
  description: 'Everything a small group needs to try the product.',
  highlighted: false,
  prices: {},
  limits: {
    seats: { count: 3, enforcement: 'hard' },
    creative_concepts: { count: 3, enforcement: 'hard' },
  },
}

const PRO_PLAN = {
  key: 'pro',
  name: 'Pro',
  description: 'For a team working together.',
  highlighted: true,
  prices: {
    monthly: {
      stripe_price_id: 'price_replace_me_pro_monthly',
      amount: 2900,
      currency: 'usd',
      per_seat: true,
    },
  },
  limits: { seats: { count: 25, enforcement: 'hard' }},
}

/** Stubs the three requests the page makes, with one billing body. */
function stubBilling(billing: unknown) {
  stubFetch({
    '/auth/me': {
      status: 200,
      body: { user: USER },
    },
    '/tenancy/memberships': {
      status: 200,
      body: MEMBERSHIPS,
    },
    [`/billing/organizations/${ORGANIZATION_ID}`]: {
      status: 200,
      body: billing,
    },
  })
}

describe('BillingPage', () => {
  it('should explain itself and offer nothing to buy when Stripe is not configured', async () => {
    stubBilling({
      plan: FREE_PLAN,
      subscription: null,
      billing_enabled: false,
      seats_used: 1,
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Billing is not configured')).toBeDefined()
    expect(screen.getByText(/Set STRIPE_SECRET_KEY/)).toBeDefined()
    // Nothing can be bought, so the grid that starts a checkout is not drawn.
    expect(screen.queryByRole('heading', { name: 'Plans' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Manage billing' })).toBeNull()
  })

  it('should show the free plan with its seat usage and every plan on offer', async () => {
    stubBilling({
      plan: FREE_PLAN,
      subscription: null,
      billing_enabled: true,
      seats_used: 2,
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Current plan: Free')).toBeDefined()
    expect(screen.getByText('2 of 3 in use')).toBeDefined()
    expect(screen.getByText('No subscription')).toBeDefined()

    // The grid comes from the generated catalog rather than from the response,
    // so both plans are offered and the one in force is marked.
    expect(screen.getByRole('heading', { name: 'Plans' })).toBeDefined()
    expect(screen.getByRole('heading', { name: 'Pro' })).toBeDefined()
    expect(screen.getByText('Most popular')).toBeDefined()
    expect(screen.getByText('per seat, per month')).toBeDefined()
    // A price the catalog carries renders without an API call for it.
    expect(screen.getByText('$29.00')).toBeDefined()

    // An admin may buy either of Pro's intervals.
    const choices = screen.getAllByRole('button', { name: 'Choose' })
    expect(choices).toHaveLength(2)
    expect(choices.some((button) => button.hasAttribute('disabled'))).toBe(false)

    // Nothing to manage at Stripe until something is bought.
    expect(screen.queryByRole('button', { name: 'Manage billing' })).toBeNull()
    expect(screen.getByRole('button', { name: 'Refresh from Stripe' })).toBeDefined()
  })

  it('should mark a live subscription that is set to end, and offer the portal', async () => {
    stubBilling({
      plan: PRO_PLAN,
      subscription: {
        id: '3f1a2b3c-0000-4000-8000-0000000000bb',
        organization_id: ORGANIZATION_ID,
        plan_key: 'pro',
        stripe_subscription_id: 'sub_test',
        status: 'active',
        billing_interval: 'monthly',
        quantity: 2,
        current_period_end: '2026-09-13T00:00:00Z',
        cancel_at_period_end: true,
        created_at: '2026-08-13T00:00:00Z',
        updated_at: '2026-08-13T00:00:00Z',
      },
      billing_enabled: true,
      seats_used: 2,
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Current plan: Pro')).toBeDefined()
    expect(screen.getByText('Active')).toBeDefined()
    expect(screen.getByText(/set to end when the current period does/)).toBeDefined()
    expect(screen.getByText('Access ends')).toBeDefined()
    expect(screen.getByRole('button', { name: 'Manage billing' })).toBeDefined()

    // The plan in force is marked in the grid, and cannot be bought again:
    // changing an existing subscription is the customer portal's job.
    await waitFor(() => {
      expect(screen.getByText('Current plan')).toBeDefined()
    })
    // Both of Pro's intervals, and neither of them buyable.
    const choices = screen.getAllByRole('button', { name: 'Choose' })
    expect(choices).toHaveLength(2)
    expect(choices.every((button) => button.hasAttribute('disabled'))).toBe(true)
  })
})
