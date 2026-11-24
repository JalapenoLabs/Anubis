// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import { createBillingRoutes } from './billingRoutes'

const ORGANIZATION_ID = '3f1a2b3c-0000-4000-8000-000000000001'

/** A wallet with auto-recharge on and a card saved, as the API answers it. */
const WIRE_WALLET = {
  currency: 'usd',
  balance_micros: 4_912_500,
  minimum_top_up_micros: 10_000_000,
  low_balance_micros: 2_000_000,
  auto_recharge: { threshold_micros: 5_000_000, amount_micros: 10_000_000 },
  card_saved: true,
  recharging: false,
  recharge_failed_at: null,
}

/** The same wallet in the shape the screens read. */
const WALLET = {
  currency: 'usd',
  balanceMicros: 4_912_500,
  minimumTopUpMicros: 10_000_000,
  lowBalanceMicros: 2_000_000,
  autoRecharge: { thresholdMicros: 5_000_000, amountMicros: 10_000_000 },
  cardSaved: true,
  recharging: false,
  rechargeFailedAt: null,
}

const WIRE_FREE_PLAN = {
  key: 'free',
  name: 'Free',
  description: null,
  highlighted: false,
  prices: {},
  limits: {},
}

type Recorded = {
  method: string
  url: string
  body: unknown
}

/** The billing routes over a fetch that records each request and answers `answer`. */
function recordingRoutes(answer: unknown) {
  const sent: Recorded[] = []
  const client = ky.create({
    prefix: 'http://localhost/',
    retry: 0,
    fetch: async (input) => {
      const request = input instanceof Request
        ? input
        : new Request(input)
      const text = await request.text()
      sent.push({
        method: request.method,
        url: request.url,
        body: text
          ? JSON.parse(text)
          : null,
      })
      return new Response(JSON.stringify(answer), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    },
  })

  return { routes: createBillingRoutes(client), sent }
}

describe('getBilling', () => {
  it('should read why the plan is in force and the wallet beside it', async () => {
    const { routes } = recordingRoutes({
      plan: WIRE_FREE_PLAN,
      plan_source: 'paid_status',
      subscription: null,
      subscriptions: [],
      billing_enabled: true,
      seats_used: 1,
      wallet: WIRE_WALLET,
    })

    const overview = await routes.getBilling(ORGANIZATION_ID)

    expect(overview.planSource).toBe('paid_status')
    expect(overview.wallet).toEqual(WALLET)
  })

  it('should keep an absent wallet absent rather than inventing an empty one', async () => {
    const { routes } = recordingRoutes({
      plan: WIRE_FREE_PLAN,
      plan_source: 'free',
      subscription: null,
      subscriptions: null,
      billing_enabled: true,
      seats_used: 1,
      wallet: null,
    })

    const overview = await routes.getBilling(ORGANIZATION_ID)

    expect(overview.wallet).toBeNull()
  })
})

describe('startTopUp', () => {
  it('should send the amount and the card choice, and answer where Stripe is', async () => {
    const { routes, sent } = recordingRoutes({ url: 'https://checkout.stripe.com/c/pay/cs_test' })

    const url = await routes.startTopUp(ORGANIZATION_ID, {
      amountMicros: 25_000_000,
      saveCard: true,
    })

    expect(sent[0]).toEqual({
      method: 'POST',
      url: `http://localhost/billing/organizations/${ORGANIZATION_ID}/wallet/top-up`,
      body: { amount_micros: 25_000_000, save_card: true },
    })
    expect(url).toBe('https://checkout.stripe.com/c/pay/cs_test')
  })
})

describe('setAutoRecharge', () => {
  it('should put the settings and answer with the wallet as it now stands', async () => {
    const { routes, sent } = recordingRoutes(WIRE_WALLET)

    const wallet = await routes.setAutoRecharge(ORGANIZATION_ID, {
      thresholdMicros: 5_000_000,
      amountMicros: 10_000_000,
    })

    expect(sent[0]).toEqual({
      method: 'PUT',
      url: `http://localhost/billing/organizations/${ORGANIZATION_ID}/wallet/auto-recharge`,
      body: { threshold_micros: 5_000_000, amount_micros: 10_000_000 },
    })
    expect(wallet).toEqual(WALLET)
  })
})

describe('disableAutoRecharge', () => {
  it('should delete the settings and answer with a wallet that has none', async () => {
    const { routes, sent } = recordingRoutes({ ...WIRE_WALLET, auto_recharge: null })

    const wallet = await routes.disableAutoRecharge(ORGANIZATION_ID)

    expect(sent[0]).toEqual({
      method: 'DELETE',
      url: `http://localhost/billing/organizations/${ORGANIZATION_ID}/wallet/auto-recharge`,
      body: null,
    })
    expect(wallet.autoRecharge).toBeNull()
    expect(wallet.cardSaved).toBe(true)
  })
})

describe('listWalletEntries', () => {
  it('should ask for one page and flatten its pagination beside the entries', async () => {
    const { routes, sent } = recordingRoutes({
      entries: [
        {
          id: '3f1a2b3c-0000-4000-8000-000000000201',
          organization_id: ORGANIZATION_ID,
          position: 7,
          kind: 'spend',
          amount_micros: -87_500,
          balance_after_micros: 4_912_500,
          requested_micros: 87_500,
          idempotency_key: 'server:1:hour:2026-11-24T12',
          stripe_payment_intent_id: null,
          description: 'Server hours',
          subject_type: 'Server',
          subject_id: '3f1a2b3c-0000-4000-8000-000000000301',
          created_at: '2026-11-24T12:00:00Z',
        },
      ],
      pagination: { page: 2, limit: 10, total_items: 11, total_pages: 2 },
    })

    const page = await routes.listWalletEntries(ORGANIZATION_ID, 2, 10)

    expect(sent[0].url).toBe(
      `http://localhost/billing/organizations/${ORGANIZATION_ID}/wallet/entries?page=2&limit=10`,
    )
    expect(page).toEqual({
      entries: [
        {
          id: '3f1a2b3c-0000-4000-8000-000000000201',
          organizationId: ORGANIZATION_ID,
          position: 7,
          kind: 'spend',
          amountMicros: -87_500,
          balanceAfterMicros: 4_912_500,
          requestedMicros: 87_500,
          idempotencyKey: 'server:1:hour:2026-11-24T12',
          stripePaymentIntentId: null,
          description: 'Server hours',
          subjectType: 'Server',
          subjectId: '3f1a2b3c-0000-4000-8000-000000000301',
          createdAt: '2026-11-24T12:00:00Z',
        },
      ],
      page: 2,
      limit: 10,
      totalItems: 11,
      totalPages: 2,
    })
  })
})
