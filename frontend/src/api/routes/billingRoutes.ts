// Copyright © 2026 Jalapeno Labs

import type {
  AutoRechargeSettings,
  BillingCheckoutRequest,
  BillingOverview,
  BillingPlan,
  BillingPlanSource,
  BillingSubscription,
  BillingWallet,
  TopUpRequest,
  WalletEntriesPage,
  WalletEntry,
  WalletEntryKind,
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

type WireSubscriptionItem = {
  id: string
  stripe_item_id: string
  stripe_price_id: string
  quantity: number | null
}

type WireSubscription = {
  id: string
  organization_id: string
  plan_key: string | null
  subject_type: string | null
  subject_id: string | null
  stripe_subscription_id: string
  status: string
  billing_interval: string
  quantity: number
  current_period_end: string | null
  cancel_at_period_end: boolean
  items: WireSubscriptionItem[]
  created_at: string
  updated_at: string
}

type WireAutoRecharge = {
  threshold_micros: number
  amount_micros: number
}

type WireWallet = {
  currency: string
  balance_micros: number
  minimum_top_up_micros: number
  low_balance_micros: number | null
  auto_recharge: WireAutoRecharge | null
  card_saved: boolean
  recharging: boolean
  recharge_failed_at: string | null
}

type WireWalletEntry = {
  id: string
  organization_id: string
  position: number
  kind: WalletEntryKind
  amount_micros: number
  balance_after_micros: number
  requested_micros: number | null
  idempotency_key: string
  stripe_payment_intent_id: string | null
  description: string | null
  subject_type: string | null
  subject_id: string | null
  created_at: string
}

type WireWalletEntriesPage = {
  entries: WireWalletEntry[]
  pagination: {
    page: number
    limit: number
    total_items: number
    total_pages: number
  }
}

type WireBilling = {
  plan: WirePlan
  plan_source: BillingPlanSource
  subscription: WireSubscription | null
  subscriptions: WireSubscription[] | null
  billing_enabled: boolean
  seats_used: number
  wallet: WireWallet | null
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
    subjectType: wire.subject_type,
    subjectId: wire.subject_id,
    stripeSubscriptionId: wire.stripe_subscription_id,
    status: wire.status,
    interval: wire.billing_interval,
    quantity: wire.quantity,
    currentPeriodEnd: wire.current_period_end,
    cancelAtPeriodEnd: wire.cancel_at_period_end,
    items: wire.items.map((item) => ({
      id: item.id,
      stripeItemId: item.stripe_item_id,
      stripePriceId: item.stripe_price_id,
      quantity: item.quantity,
    })),
    createdAt: wire.created_at,
    updatedAt: wire.updated_at,
  }
}

function toWallet(wire: WireWallet): BillingWallet {
  return {
    currency: wire.currency,
    balanceMicros: wire.balance_micros,
    minimumTopUpMicros: wire.minimum_top_up_micros,
    lowBalanceMicros: wire.low_balance_micros,
    autoRecharge: wire.auto_recharge
      ? {
          thresholdMicros: wire.auto_recharge.threshold_micros,
          amountMicros: wire.auto_recharge.amount_micros,
        }
      : null,
    cardSaved: wire.card_saved,
    recharging: wire.recharging,
    rechargeFailedAt: wire.recharge_failed_at,
  }
}

function toWalletEntry(wire: WireWalletEntry): WalletEntry {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    position: wire.position,
    kind: wire.kind,
    amountMicros: wire.amount_micros,
    balanceAfterMicros: wire.balance_after_micros,
    requestedMicros: wire.requested_micros,
    idempotencyKey: wire.idempotency_key,
    stripePaymentIntentId: wire.stripe_payment_intent_id,
    description: wire.description,
    subjectType: wire.subject_type,
    subjectId: wire.subject_id,
    createdAt: wire.created_at,
  }
}

function toOverview(wire: WireBilling): BillingOverview {
  return {
    plan: toPlan(wire.plan),
    planSource: wire.plan_source,
    subscription: wire.subscription
      ? toSubscription(wire.subscription)
      : null,
    subscriptions: wire.subscriptions
      ? wire.subscriptions.map((subscription) => toSubscription(subscription))
      : null,
    billingEnabled: wire.billing_enabled,
    seatsUsed: wire.seats_used,
    wallet: wire.wallet
      ? toWallet(wire.wallet)
      : null,
  }
}

/**
 * The organization's plan, subscriptions, and prepaid credit, and the Stripe
 * redirects that buy them.
 *
 * Money lives at Stripe: buying a plan, managing one, and buying credit all
 * answer with a URL to send the browser to, and nothing here renders a card
 * form. Reading the plan is open to every member; every write, the wallet,
 * and its ledger need the `admin` or `billing` role. A subscription paying for
 * one of the application's records is bought and changed through the
 * application's own routes.
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

  /**
   * Opens a Stripe Checkout session that buys credit; the caller sends the
   * browser to its URL.
   *
   * Nothing is credited by this call. The credit lands when Stripe's payment
   * event does, for the amount Stripe actually received.
   */
  async function startTopUp(organizationId: string, request: TopUpRequest): Promise<string> {
    const response = await client
      .post(`billing/organizations/${organizationId}/wallet/top-up`, {
        json: {
          amount_micros: request.amountMicros,
          save_card: request.saveCard,
        },
      })
      .json<{ url: string }>()
    return response.url
  }

  /**
   * Turns auto-recharge on, or changes it, answering with the wallet.
   *
   * Refused with `409` until a top-up saved a card, because the saved card is
   * the only one a recharge may charge without the person present.
   */
  async function setAutoRecharge(
    organizationId: string,
    settings: AutoRechargeSettings,
  ): Promise<BillingWallet> {
    const response = await client
      .put(`billing/organizations/${organizationId}/wallet/auto-recharge`, {
        json: {
          threshold_micros: settings.thresholdMicros,
          amount_micros: settings.amountMicros,
        },
      })
      .json<WireWallet>()
    return toWallet(response)
  }

  /** Turns auto-recharge off, answering with the wallet. The saved card stays saved. */
  async function disableAutoRecharge(organizationId: string): Promise<BillingWallet> {
    const response = await client
      .delete(`billing/organizations/${organizationId}/wallet/auto-recharge`)
      .json<WireWallet>()
    return toWallet(response)
  }

  /**
   * One page of the wallet's ledger, newest first.
   *
   * A read like the overview, so it answers while Stripe is unset; it is gated
   * on the billing roles because a spend names the record it paid for.
   */
  async function listWalletEntries(
    organizationId: string,
    page = 1,
    limit = 25,
  ): Promise<WalletEntriesPage> {
    const response = await client
      .get(`billing/organizations/${organizationId}/wallet/entries`, {
        searchParams: {
          page,
          limit,
        },
      })
      .json<WireWalletEntriesPage>()

    return {
      entries: response.entries.map(toWalletEntry),
      page: response.pagination.page,
      limit: response.pagination.limit,
      totalItems: response.pagination.total_items,
      totalPages: response.pagination.total_pages,
    }
  }

  return {
    getBilling,
    startCheckout,
    openBillingPortal,
    reconcileBilling,
    startTopUp,
    setAutoRecharge,
    disableAutoRecharge,
    listWalletEntries,
  } as const
}
