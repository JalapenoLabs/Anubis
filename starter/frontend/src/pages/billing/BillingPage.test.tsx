// Copyright © 2026 Jalapeno Labs

// UI
import { App } from '../../App'

// Utility
import { describe, expect, it } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

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

/** What an administrator holds, as the overview answers it. */
const ADMINISTRATOR = {
  roles: [{ id: 'role-admin', name: 'Administrator', color: '#e11d48', position: 3 }],
  permissions: [ 'Billing::Subscription.update' ],
  rank: 3,
  is_administrator: true,
}

/** One organization the account administers, with one team in it. */
const MEMBERSHIPS = {
  organizations: [
    {
      id: ORGANIZATION_ID,
      name: 'Acme',
      is_member: true,
      ...ADMINISTRATOR,
      teams: [
        {
          id: '3f1a2b3c-0000-4000-8000-0000000000aa',
          name: 'Acme HQ',
          ...ADMINISTRATOR,
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

/**
 * A wallet holding $25.00 with auto-recharge on, as the API answers it.
 *
 * `config/billing.yml` sells credit with a $10.00 minimum top-up and a $2.00
 * low-balance warning, which is what these mirror.
 */
const WALLET = {
  currency: 'usd',
  balance_micros: 25_000_000,
  minimum_top_up_micros: 10_000_000,
  charge_step_micros: 10_000,
  low_balance_micros: 2_000_000,
  auto_recharge: { threshold_micros: 5_000_000, amount_micros: 10_000_000 },
  card_saved: true,
  recharging: false,
  recharge_failed_at: null,
}

/** The free plan with nothing bought, which the wallet cases build on. */
const FREE_BILLING = {
  plan: FREE_PLAN,
  plan_source: 'free',
  subscription: null,
  subscriptions: [],
  billing_enabled: true,
  seats_used: 1,
  wallet: null,
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
      plan_source: 'free',
      subscription: null,
      subscriptions: [],
      billing_enabled: false,
      seats_used: 1,
      wallet: null,
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
      plan_source: 'free',
      subscription: null,
      subscriptions: [],
      billing_enabled: true,
      seats_used: 2,
      wallet: null,
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
    const planSubscription = {
      id: '3f1a2b3c-0000-4000-8000-0000000000bb',
      organization_id: ORGANIZATION_ID,
      plan_key: 'pro',
      subject_type: null,
      subject_id: null,
      stripe_subscription_id: 'sub_test',
      status: 'active',
      billing_interval: 'monthly',
      quantity: 2,
      current_period_end: '2026-09-13T00:00:00Z',
      cancel_at_period_end: true,
      items: [
        {
          id: '3f1a2b3c-0000-4000-8000-0000000000cc',
          subscription_id: '3f1a2b3c-0000-4000-8000-0000000000bb',
          stripe_item_id: 'si_test',
          stripe_price_id: 'price_replace_me_pro_monthly',
          quantity: 2,
          created_at: '2026-08-13T00:00:00Z',
          updated_at: '2026-08-13T00:00:00Z',
        },
      ],
      created_at: '2026-08-13T00:00:00Z',
      updated_at: '2026-08-13T00:00:00Z',
    }
    stubBilling({
      plan: PRO_PLAN,
      plan_source: 'plan_subscription',
      subscription: planSubscription,
      subscriptions: [ planSubscription ],
      billing_enabled: true,
      seats_used: 2,
      wallet: null,
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

  it('should show the balance, how it refills, and the ways to add credit', async () => {
    stubBilling({ ...FREE_BILLING, wallet: WALLET })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByRole('heading', { name: 'Credit' })).toBeDefined()
    expect(screen.getByText('$25.00')).toBeDefined()
    expect(screen.getByText('Recharges $10.00 at or below $5.00')).toBeDefined()
    expect(screen.getByRole('button', { name: 'Add credit' })).toBeDefined()
    expect(screen.getByRole('button', { name: 'Auto-recharge' })).toBeDefined()
    // A real link, so it opens in a new tab like any other.
    expect(screen.getByRole('link', { name: 'View activity' }).getAttribute('href'))
      .toBe(`${BILLING_PATH}/activity`)
    // Well above the low-balance line, so nothing warns.
    expect(screen.queryByText(/running low/)).toBeNull()
  })

  it('should open Add credit on the smallest top-up, with the cursor in it', async () => {
    stubBilling({ ...FREE_BILLING, wallet: WALLET })

    renderWithProviders(<App />, BILLING_PATH)

    fireEvent.click(await screen.findByRole('button', { name: 'Add credit' }))

    const amount = await screen.findByLabelText('Amount')
    await waitFor(() => {
      expect(amount).toHaveProperty('value', '10')
    })
    expect(document.activeElement).toBe(amount)
    expect(screen.getByRole('button', { name: 'Continue to Stripe' })).toBeDefined()
  })

  it('should open Auto-recharge on the settings in force', async () => {
    stubBilling({ ...FREE_BILLING, wallet: WALLET })

    renderWithProviders(<App />, BILLING_PATH)

    fireEvent.click(await screen.findByRole('button', { name: 'Auto-recharge' }))

    const threshold = await screen.findByLabelText('Balance threshold')
    await waitFor(() => {
      expect(threshold).toHaveProperty('value', '5')
    })
    expect(screen.getByLabelText('Recharge amount')).toHaveProperty('value', '10')
    expect(document.activeElement).toBe(threshold)
  })

  it('should draw no credit card for an application that sells none', async () => {
    stubBilling(FREE_BILLING)

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Current plan: Free')).toBeDefined()
    expect(screen.queryByRole('heading', { name: 'Credit' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Add credit' })).toBeNull()
  })

  it('should still show the balance and its activity, but nothing to buy, without Stripe', async () => {
    stubBilling({
      ...FREE_BILLING,
      billing_enabled: false,
      wallet: { ...WALLET, balance_micros: 1_500_000 },
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('$1.50')).toBeDefined()
    expect(screen.getByText(/running low/)).toBeDefined()
    // Reading the ledger needs no Stripe; both writes would answer 503.
    expect(screen.getByRole('link', { name: 'View activity' })).toBeDefined()
    expect(screen.queryByRole('button', { name: 'Add credit' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Auto-recharge' })).toBeNull()
  })

  it('should say a refused recharge turned auto-recharge off, and a balance below zero', async () => {
    stubBilling({
      ...FREE_BILLING,
      wallet: {
        ...WALLET,
        balance_micros: -1_500_000,
        auto_recharge: null,
        recharge_failed_at: '2026-11-20T12:00:00Z',
      },
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('-$1.50')).toBeDefined()
    expect(screen.getByText('Auto-recharge is off')).toBeDefined()
    expect(screen.getByText(/The recharge on November 20, 2026 was declined/)).toBeDefined()
    expect(screen.getByText(/below zero after a refund/)).toBeDefined()
    // Below zero says more than low, so only one of the two warns.
    expect(screen.queryByText(/running low/)).toBeNull()
  })

  it('should keep quiet about a past refusal once auto-recharge is back on', async () => {
    stubBilling({
      ...FREE_BILLING,
      wallet: { ...WALLET, recharge_failed_at: '2026-11-20T12:00:00Z' },
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Recharges $10.00 at or below $5.00')).toBeDefined()
    expect(screen.queryByText(/was declined/)).toBeNull()
  })

  it('should say the plan comes with what the organization pays for', async () => {
    stubBilling({
      ...FREE_BILLING,
      plan: PRO_PLAN,
      plan_source: 'paid_status',
      wallet: WALLET,
    })

    renderWithProviders(<App />, BILLING_PATH)

    expect(await screen.findByText('Current plan: Pro')).toBeDefined()
    expect(screen.getByText('Included')).toBeDefined()
    expect(screen.getByText(/comes with any subscription or credit/)).toBeDefined()
    expect(screen.queryByText('No subscription')).toBeNull()
  })

  it('should open on the sentence for a top-up Stripe returned from', async () => {
    stubBilling({ ...FREE_BILLING, wallet: WALLET })

    renderWithProviders(<App />, `${BILLING_PATH}?top_up=success`)

    expect(await screen.findByText(/as soon as Stripe confirms the payment/)).toBeDefined()
    // A top-up is not a subscription, so the checkout's sentence stays away.
    expect(screen.queryByText(/setting the subscription up/)).toBeNull()
  })

  it('should say a cancelled top-up charged nothing', async () => {
    stubBilling({ ...FREE_BILLING, wallet: WALLET })

    renderWithProviders(<App />, `${BILLING_PATH}?top_up=canceled`)

    expect(await screen.findByText('The top-up was cancelled. Nothing was charged.')).toBeDefined()
  })
})
