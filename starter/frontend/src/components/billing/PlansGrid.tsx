// Copyright © 2026 Jalapeno Labs

import type { BillingInterval } from '../../plans.generated'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { PlanCard } from './PlanCard'

// Misc
import { Plans } from '../../plans.generated'

type Props = {
  organizationId: string
  /** The plan in force, so the grid can mark it. */
  currentPlanKey: string
  /** True for an organization admin, or anyone holding the billing role. */
  canManage: boolean
}

/**
 * Every plan this application sells, in the order `config/billing.yml` lists it.
 *
 * The catalog comes from `plans.generated.ts` rather than from an endpoint,
 * because it is configuration compiled into this same build: asking the server
 * for it would be a request for a file that shipped with the page.
 *
 * Choosing a plan opens a Stripe Checkout session and leaves the SPA for it.
 * Changing an existing subscription is the customer portal's job, so a plan
 * card is disabled once the organization is on it.
 */
export function PlansGrid(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ isBusy, setIsBusy ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  async function onChoose(planKey: string, interval: BillingInterval) {
    setIsBusy(true)
    setErrorMessage(null)
    try {
      window.location.href = await api.startCheckout(props.organizationId, {
        planKey,
        interval,
      })
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
      setIsBusy(false)
    }
  }

  return <section className='relaxed'>
    <div className='relaxed'>
      <h3 className='text-xl font-semibold'>{
          t('billing.plansTitle')
        }</h3>
      <p className='opacity-70'>{
          props.canManage
            ? t('billing.plansSubtitle')
            : t('billing.plansReadOnly')
        }</p>
    </div>
    <div className='grid gap-4 sm:grid-cols-2'>
      {
        Plans.map((plan) => (
          <PlanCard
            key={plan.key}
            plan={plan}
            isCurrent={plan.key === props.currentPlanKey}
            canManage={props.canManage}
            isBusy={isBusy}
            onChoose={(interval) => onChoose(plan.key, interval)}
          />
        ))
      }
    </div>
    { errorMessage
      ? <p className='mt-4 text-danger'>{
          errorMessage
        }</p>
      : null
    }
  </section>
}
