// Copyright © 2026 Jalapeno Labs

import type { BillingOverview } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody, Chip } from '@heroui/react'

// Misc
import { findPlan } from '../../plans.generated'

type Props = {
  organizationId: string
  overview: BillingOverview
  /** True for an organization admin, or anyone holding the billing role. */
  canManage: boolean
  /** Revalidates the screen after Stripe was read again. */
  onReconciled: () => Promise<unknown>
}

type StatusColor = 'primary' | 'success' | 'warning' | 'danger' | 'default'

/**
 * Stripe's status vocabulary, and the colour each one deserves.
 *
 * `past_due` is amber rather than red on purpose: Stripe is still retrying the
 * card and the customer still has access, so the screen nudges rather than
 * alarms. The status arrives as a string from Stripe's own vocabulary rather
 * than from an enum this build owns, so a word a later API version adds is
 * looked up and simply renders neutral.
 */
const colorByStatus: Record<string, StatusColor | undefined> = {
  trialing: 'primary',
  active: 'success',
  past_due: 'warning',
  incomplete: 'warning',
  unpaid: 'danger',
  paused: 'default',
  canceled: 'default',
  incomplete_expired: 'default',
}

/** The plan in force, what it includes, and the ways to change it. */
export function CurrentPlanCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ isBusy, setIsBusy ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const { plan, subscription, seatsUsed } = props.overview
  const status = subscription?.status ?? ''
  const seatsLimit = plan.limits.seats
  const isOverSeats = Boolean(seatsLimit && seatsUsed > seatsLimit.count)

  // The generated catalog carries the copy a pricing page shows; the API
  // carries the plan actually in force. They are the same file, so a plan the
  // catalog no longer lists is a deployment mid-rollout rather than an error.
  const described = findPlan(plan.key)

  async function onOpenPortal() {
    setIsBusy(true)
    setErrorMessage(null)
    try {
      window.location.href = await api.openBillingPortal(props.organizationId)
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
      setIsBusy(false)
    }
  }

  async function onReconcile() {
    setIsBusy(true)
    setErrorMessage(null)
    try {
      await api.reconcileBilling(props.organizationId)
      await props.onReconciled()
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsBusy(false)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <div className='level items-start'>
        <div>
          <h3 className='compact text-xl font-semibold'>{
              t('billing.currentPlanTitle', { plan: plan.name })
            }</h3>
          <p className='opacity-70'>{
              described?.description ?? plan.description ?? t('billing.freeSubtitle')
            }</p>
        </div>
        { subscription
          ? <Chip color={colorByStatus[status] ?? 'default'} variant='flat'>{
              t(`billing.statuses.${status}`, { defaultValue: status })
            }</Chip>
          : <Chip variant='flat'>{
              t('billing.noSubscription')
            }</Chip>
        }
      </div>

      <dl className='compact mt-4 grid gap-2 sm:grid-cols-2'>
        <div>
          <dt className='text-sm opacity-70'>{
              t('billing.seatsLabel')
            }</dt>
          <dd>{
              seatsLimit
                ? t('billing.seatsOfLimit', { used: seatsUsed, allowed: seatsLimit.count })
                : t('billing.seatsUnlimited', { used: seatsUsed })
            }</dd>
        </div>
        { subscription?.currentPeriodEnd
          ? <div>
              <dt className='text-sm opacity-70'>{
                  subscription.cancelAtPeriodEnd
                    ? t('billing.accessEndsLabel')
                    : t('billing.renewsLabel')
                }</dt>
              <dd>{
                  new Date(subscription.currentPeriodEnd).toLocaleDateString()
                }</dd>
            </div>
          : null
        }
      </dl>

      { isOverSeats
        ? <p className='mt-2 text-warning'>{
            t('billing.overSeats')
          }</p>
        : null
      }
      { status === 'past_due'
        ? <p className='mt-2 text-warning'>{
            t('billing.pastDueWarning')
          }</p>
        : null
      }
      { subscription?.cancelAtPeriodEnd
        ? <p className='mt-2 opacity-80'>{
            t('billing.cancelAtPeriodEnd')
          }</p>
        : null
      }

      { props.canManage && props.overview.billingEnabled
        ? <div className='level-left mt-4 gap-2'>
            { subscription
              ? <Button color='primary' isDisabled={isBusy} onPress={onOpenPortal}>
                  <span>{
                      t('billing.managePortal')
                    }</span>
                </Button>
              : null
            }
            <Button variant='light' isDisabled={isBusy} onPress={onReconcile}>
              <span>{
                  t('billing.refresh')
                }</span>
            </Button>
          </div>
        : null
      }
      { errorMessage
        ? <p className='mt-4 text-danger'>{
            errorMessage
          }</p>
        : null
      }
    </CardBody>
  </Card>
}
