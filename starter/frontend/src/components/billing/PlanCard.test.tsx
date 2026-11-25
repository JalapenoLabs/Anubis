// Copyright © 2026 Jalapeno Labs

import type { Plan } from '../../plans.generated'

// UI
import { PlanCard } from './PlanCard'

// Utility
import { describe, expect, it, vi } from 'vitest'
import { screen } from '@testing-library/react'

// Misc
import { renderWithProviders } from '../../testing/harness'

/**
 * A paid plan that sells no price, which an application names as `paid_plan`.
 *
 * The starter's own `billing.yml` names none, so the generated catalog never
 * renders one and this is the only place the case is drawn.
 */
const PAID_PLAN: Plan = {
  key: 'pro',
  name: 'Usage',
  description: 'Everything, for as long as the organization pays for something.',
  highlighted: false,
  free: false,
  paid: true,
  prices: {},
  limits: { seats: { count: 25, enforcement: 'hard' }},
}

function renderPlan(plan: Plan) {
  renderWithProviders(
    <PlanCard
      plan={plan}
      isCurrent={false}
      canManage
      isBusy={false}
      onChoose={vi.fn()}
    />,
  )
}

describe('PlanCard', () => {
  it('should read the paid plan as coming with what the organization pays for', () => {
    renderPlan(PAID_PLAN)

    expect(screen.getByText('Included with any subscription or credit')).toBeDefined()
    // Neither a price nor "Free", and nothing to buy.
    expect(screen.queryByText('Free')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Choose' })).toBeNull()
  })

  it('should sell a paid plan that has prices of its own', () => {
    renderPlan({
      ...PAID_PLAN,
      prices: {
        monthly: {
          stripePriceId: 'price_usage_monthly',
          amount: 900,
          currency: 'usd',
          perSeat: false,
        },
      },
    })

    expect(screen.queryByText('Included with any subscription or credit')).toBeNull()
    expect(screen.getByRole('button', { name: 'Choose' })).toBeDefined()
  })

  it('should still read the free plan as free', () => {
    renderPlan({ ...PAID_PLAN, name: 'Starter', free: true, paid: false })

    expect(screen.getByText('Free')).toBeDefined()
    expect(screen.queryByText('Included with any subscription or credit')).toBeNull()
  })
})
