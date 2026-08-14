// Copyright © 2026 Jalapeno Labs

import type { BillingInterval, Plan } from '../../plans.generated'

// Core
import { useTranslation } from 'react-i18next'

// UI
import { Button, Card, CardBody, Chip } from '@heroui/react'

type Props = {
  plan: Plan
  /** True for the plan the organization is on right now. */
  isCurrent: boolean
  /** True for an organization admin, or anyone holding the billing role. */
  canManage: boolean
  /** True while another checkout is opening, so one click cannot start two. */
  isBusy: boolean
  onChoose: (interval: BillingInterval) => void
}

/** The intervals a card offers, in the order a pricing page offers them. */
const INTERVALS: readonly BillingInterval[] = [ 'monthly', 'yearly' ]

/**
 * Renders an amount in the currency's own formatting.
 *
 * Amounts are stored in the smallest unit, as Stripe stores them, so 2900 usd
 * is $29.00. `Intl` knows how many of those units a currency has, which is what
 * keeps a zero-decimal currency such as jpy from being divided by a hundred.
 */
function formatAmount(amount: number, currency: string): string {
  const formatter = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: currency.toUpperCase(),
  })
  const digits = formatter.resolvedOptions().maximumFractionDigits ?? 2

  return formatter.format(amount / 10 ** digits)
}

/** One plan of the pricing grid: what it costs, what it includes, and how to buy it. */
export function PlanCard(props: Props) {
  const { t } = useTranslation()
  const { plan } = props

  // Full height with the price pushed to the bottom, so the buttons of every
  // card in the grid sit on one line however much a plan has to say for itself.
  return <Card
    className={
      plan.highlighted
        ? 'h-full p-2 border-2 border-primary'
        : 'h-full p-2'
    }
  >
    <CardBody className='flex flex-col'>
      <div className='level items-start'>
        <h4 className='text-lg font-semibold'>{
            plan.name
          }</h4>
        { props.isCurrent
          ? <Chip color='success' variant='flat'>{
              t('billing.currentBadge')
            }</Chip>
          : null
        }
        { plan.highlighted && !props.isCurrent
          ? <Chip color='primary' variant='flat'>{
              t('billing.highlightedBadge')
            }</Chip>
          : null
        }
      </div>
      <p className='compact opacity-70'>{
          plan.description ?? ''
        }</p>

      <ul className='compact'>
        {
          Object.entries(plan.limits).map(([ name, limit ]) => (
            <li key={name} className='text-sm opacity-80'>{
                t('billing.limitLine', {
                  count: limit?.count ?? 0,
                  // Limit names are the application's own vocabulary, written
                  // in snake case because they cross into JSON and TypeScript.
                  limit: name.replaceAll('_', ' '),
                })
              }{
                limit?.enforcement === 'soft'
                  ? ` ${t('billing.softLimitSuffix')}`
                  : ''
              }</li>
          ))
        }
      </ul>

      { plan.free
        ? <p className='mt-auto pt-4 text-lg font-semibold'>{
            t('billing.freePrice')
          }</p>
        : <div className='mt-auto pt-4'>
            {
              INTERVALS.map((interval) => {
                const price = plan.prices[interval]
                if (!price) {
                  return null
                }

                return <div key={interval} className='compact'>
                  <p className='text-lg font-semibold'>{
                      formatAmount(price.amount, price.currency)
                    }</p>
                  <p className='text-sm opacity-70'>{
                      price.perSeat
                        ? t(`billing.perSeat.${interval}`)
                        : t(`billing.per.${interval}`)
                    }</p>
                  <Button
                    className='mt-2 w-full'
                    color={plan.highlighted ? 'primary' : 'default'}
                    isDisabled={!props.canManage || props.isBusy || props.isCurrent}
                    onPress={() => props.onChoose(interval)}
                  >
                    <span>{
                        t('billing.chooseAction')
                      }</span>
                  </Button>
                </div>
              })
            }
          </div>
      }
    </CardBody>
  </Card>
}
