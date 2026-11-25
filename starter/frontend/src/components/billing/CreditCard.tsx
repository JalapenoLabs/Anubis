// Copyright © 2026 Jalapeno Labs

import type { BillingWallet } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { AutoRechargeModal } from './AutoRechargeModal'
import { TopUpModal } from './TopUpModal'

// Misc
import { formatMicros } from '../../money'
import { getOrganizationBillingActivityUrl } from '../../urls'

type Props = {
  organizationId: string
  /** Arrives only for the `admin` and `billing` roles, so whoever sees it may spend. */
  wallet: BillingWallet
  /** False when this deployment has no Stripe key, so both writes would answer `503`. */
  billingEnabled: boolean
  /** Revalidates the screen after auto-recharge changed. */
  onChanged: () => Promise<unknown>
}

/**
 * The organization's prepaid credit: the balance, how it refills, and the ways to add to it.
 *
 * Without Stripe the balance still shows and the ledger still reads, because
 * both are this application's own rows; only the two buttons that would reach
 * Stripe are left out, the same way the billing screen leaves out the grid.
 */
export function CreditCard(props: Props) {
  const { t } = useTranslation()
  const [ isTopUpOpen, setIsTopUpOpen ] = useState(false)
  const [ isAutoRechargeOpen, setIsAutoRechargeOpen ] = useState(false)

  const { wallet } = props
  const { currency, balanceMicros, lowBalanceMicros, autoRecharge } = wallet

  // Below zero says more than low, so the two never show together: a refund of
  // credit already spent is the only way a balance gets there.
  const isBelowZero = balanceMicros < 0
  const isLow = !isBelowZero && lowBalanceMicros !== null && balanceMicros < lowBalanceMicros
  // The server turns auto-recharge off when a recharge is refused, and on again
  // only when someone saves it, so a failure with it back on is history.
  const rechargeFailedAt = autoRecharge
    ? null
    : wallet.rechargeFailedAt

  return <Card className='relaxed p-2'>
    <CardBody>
      <div className='level items-start'>
        <div>
          <h3 className='compact text-xl font-semibold'>{
              t('billing.wallet.title')
            }</h3>
          <p
            className={
              isBelowZero
                ? 'text-3xl font-semibold text-danger'
                : 'text-3xl font-semibold'
            }
          >{
              formatMicros(balanceMicros, currency)
            }</p>
        </div>
        <Link
          to={getOrganizationBillingActivityUrl(props.organizationId)}
          className='shrink-0 text-primary'
        >{
            t('billing.wallet.activityLink')
          }</Link>
      </div>

      { isBelowZero
        ? <p className='mt-2 text-danger'>{
            t('billing.wallet.belowZero')
          }</p>
        : null
      }
      { isLow
        ? <p className='mt-2 text-warning'>{
            t('billing.wallet.lowBalance')
          }</p>
        : null
      }

      <p className='mt-4 opacity-80'>{
          autoRecharge
            ? t('billing.wallet.autoRechargeOn', {
                amount: formatMicros(autoRecharge.amountMicros, currency),
                threshold: formatMicros(autoRecharge.thresholdMicros, currency),
              })
            : t('billing.wallet.autoRechargeOff')
        }</p>
      { wallet.recharging
        ? <p className='mt-2 opacity-80'>{
            t('billing.wallet.recharging')
          }</p>
        : null
      }
      { rechargeFailedAt
        ? <p className='mt-2 text-warning'>{
            t('billing.wallet.rechargeFailed', {
              date: new Date(rechargeFailedAt).toLocaleDateString(undefined, { dateStyle: 'long' }),
            })
          }</p>
        : null
      }

      { props.billingEnabled
        ? <>
            <div className='level-left mt-4 gap-2'>
              <Button color='primary' onPress={() => setIsTopUpOpen(true)}>
                <span>{
                    t('billing.wallet.addCredit')
                  }</span>
              </Button>
              <Button variant='flat' onPress={() => setIsAutoRechargeOpen(true)}>
                <span>{
                    t('billing.wallet.autoRecharge')
                  }</span>
              </Button>
            </div>
            <TopUpModal
              organizationId={props.organizationId}
              wallet={wallet}
              isOpen={isTopUpOpen}
              onClose={() => setIsTopUpOpen(false)}
            />
            <AutoRechargeModal
              organizationId={props.organizationId}
              wallet={wallet}
              isOpen={isAutoRechargeOpen}
              onClose={() => setIsAutoRechargeOpen(false)}
              onChanged={props.onChanged}
            />
          </>
        : null
      }
    </CardBody>
  </Card>
}
