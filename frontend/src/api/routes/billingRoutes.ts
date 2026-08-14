// Copyright © 2026 Jalapeno Labs

import type {
  BillingCheckoutRequest,
  BillingOverview,
  BillingPlan,
  BillingSubscription,
} from '../types'
import type { KyInstance } from 'ky'

type WirePlanPrice = {
  stripe_price_id: string
  amount: number
  currency: string
  per_seat: boolean
}

type WirePlanLimit = {
  count: number
  enforcement: 'hard' | 'soft'
}

type WirePlan = {
  key: string
  name: string
  description: string | null
  highlighted: boolean
  prices: Record<string, WirePlanPrice>
  limits: Record<string, WirePlanLimit>
}

type WireSubscription = {
  id: string
  organization_id: string
  plan_key: string
  stripe_subscription_id: string
  status: string
  billing_interval: string
  quantity: number
  current_period_end: string | null
  cancel_at_period_end: boolean
  created_at: string
  updated_at: string
}

type WireBilling = {
  plan: WirePlan
  subscription: WireSubscription | null
  billing_enabled: boolean
  seats_used: number
}

function toPlan(wire: WirePlan): BillingPlan {
  const prices: BillingPlan['prices'] = {}
  for (const [ interval, price ] of Object.entries(wire.prices)) {
    prices[interval] = {
      stripePriceId: price.stripe_price_id,
      amount: price.amount,
      currency: price.currency,
      perSeat: price.per_seat,
    }
  }

  return {
    key: wire.key,
    name: wire.name,
    description: wire.description,
    highlighted: wire.highlighted,
    prices,
    limits: wire.limits,
  }
}

function toSubscription(wire: WireSubscription): BillingSubscription {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    planKey: wire.plan_key,
    stripeSubscriptionId: wire.stripe_subscription_id,
    status: wire.status,
    interval: wire.billing_interval,
    quantity: wire.quantity,
    currentPeriodEnd: wire.current_period_end,
    cancelAtPeriodEnd: wire.cancel_at_period_end,
    createdAt: wire.created_at,
    updatedAt: wire.updated_at,
  }
}

function toOverview(wire: WireBilling): BillingOverview {
  return {
    plan: toPlan(wire.plan),
    subscription: wire.subscription
      ? toSubscription(wire.subscription)
      : null,
    billingEnabled: wire.billing_enabled,
    seatsUsed: wire.seats_used,
  }
}

/**
 * The organization's plan, and the two Stripe redirects that change it.
 *
 * Money lives at Stripe: buying a plan and managing one both answer with a URL
 * to send the browser to, and nothing here renders a card form. Reading is open
 * to every member; the three writes need the `admin` or `billing` role.
 */
export function createBillingRoutes(client: KyInstance) {
  async function getBilling(organizationId: string): Promise<BillingOverview> {
    const response = await client
      .get(`billing/organizations/${organizationId}`)
      .json<WireBilling>()
    return toOverview(response)
  }

  /** Opens a Stripe Checkout session; the caller sends the browser to its URL. */
  async function startCheckout(
    organizationId: string,
    request: BillingCheckoutRequest,
  ): Promise<string> {
    const response = await client
      .post(`billing/organizations/${organizationId}/checkout`, {
        json: {
          plan_key: request.planKey,
          interval: request.interval,
        },
      })
      .json<{ url: string }>()
    return response.url
  }

  /** Opens the Stripe customer portal, where every change after the purchase happens. */
  async function openBillingPortal(organizationId: string): Promise<string> {
    const response = await client
      .post(`billing/organizations/${organizationId}/portal`)
      .json<{ url: string }>()
    return response.url
  }

  /** Reads Stripe and corrects what this application shows, answering like the get. */
  async function reconcileBilling(organizationId: string): Promise<BillingOverview> {
    const response = await client
      .post(`billing/organizations/${organizationId}/reconcile`)
      .json<WireBilling>()
    return toOverview(response)
  }

  return {
    getBilling,
    startCheckout,
    openBillingPortal,
    reconcileBilling,
  } as const
}
