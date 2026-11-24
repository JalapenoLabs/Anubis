// Copyright © 2026 Jalapeno Labs

import type { AmountProblem } from '../../money'
import type { BillingWallet } from '@jalapenolabs/anubis'
import type { FormEvent } from 'react'

// Core
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Input,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
  Tooltip,
} from '@heroui/react'

// Misc
import {
  MICROS_PER_UNIT,
  chargeStepMicros,
  currencySymbol,
  findAmountProblem,
  formatMicros,
  parseMicros,
} from '../../money'

type Props = {
  organizationId: string
  wallet: BillingWallet
  isOpen: boolean
  onClose: () => void
  /** Revalidates the screen after auto-recharge was saved or turned off. */
  onChanged: () => Promise<unknown>
}

/** The sentence a disabled Save explains itself with, for a threshold problem. */
const thresholdReasonByProblem = {
  missing: 'billing.wallet.autoRechargeForm.thresholdMissing',
  belowMinimum: 'billing.wallet.autoRechargeForm.thresholdBelowZero',
  partialUnit: 'billing.wallet.partialUnit',
} as const satisfies Record<AmountProblem, string>

/** The sentence a disabled Save explains itself with, for an amount problem. */
const amountReasonByProblem = {
  missing: 'billing.wallet.autoRechargeForm.amountMissing',
  belowMinimum: 'billing.wallet.autoRechargeForm.amountBelowMinimum',
  partialUnit: 'billing.wallet.partialUnit',
} as const satisfies Record<AmountProblem, string>

/** Micro-units as the text an amount field shows, or empty for none. */
function toTyped(micros: number | null): string {
  if (micros === null) {
    return ''
  }

  return String(micros / MICROS_PER_UNIT)
}

/**
 * Sets the balance auto-recharge fires below, and what each recharge buys.
 *
 * A recharge charges the card a top-up saved, off-session, so without one the
 * form is shown disabled with the way to get one beneath it, rather than
 * hidden: the person learns what it takes before spending any effort on it.
 *
 * The checks mirror the server's, so Save is only offered for settings it will
 * take: a recharge buys at least the smallest top-up, in whole cents, and at
 * least the threshold, which is what lets one recharge lift a spent-down
 * balance back over it. The server's own refusals still land form-level.
 */
export function AutoRechargeModal(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { currency, minimumTopUpMicros, autoRecharge, cardSaved } = props.wallet

  const [ threshold, setThreshold ] = useState('')
  const [ amount, setAmount ] = useState('')
  const [ pendingAction, setPendingAction ] = useState<'save' | 'turnOff' | null>(null)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  // The modal stays mounted while closed, so each opening starts over from the
  // settings in force (they move after a save) and puts the cursor in the form.
  // Turned off, it suggests recharging the smallest top-up below the balance
  // the wallet already warns at.
  const thresholdRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (!props.isOpen) {
      return
    }

    setThreshold(toTyped(autoRecharge?.thresholdMicros ?? props.wallet.lowBalanceMicros))
    setAmount(toTyped(autoRecharge?.amountMicros ?? minimumTopUpMicros))
    setErrorMessage(null)
    thresholdRef.current?.focus()
  }, [ props.isOpen ])

  const thresholdMicros = parseMicros(threshold)
  const amountMicros = parseMicros(amount)
  const stepMicros = chargeStepMicros(currency)
  // One floor carries both of the amount's lower bounds: the smallest top-up,
  // and the threshold, so a single recharge always clears it.
  const amountFloorMicros = Math.max(minimumTopUpMicros, thresholdMicros ?? 0)

  const thresholdProblem = findAmountProblem(thresholdMicros, 0, currency)
  const amountProblem = findAmountProblem(amountMicros, amountFloorMicros, currency)

  const reasonValues = {
    minimum: formatMicros(amountFloorMicros, currency),
    step: formatMicros(stepMicros, currency),
  }
  let invalidReason: string | null = null
  if (thresholdProblem) {
    invalidReason = t(thresholdReasonByProblem[thresholdProblem], reasonValues)
  }
  else if (amountProblem) {
    invalidReason = t(amountReasonByProblem[amountProblem], reasonValues)
  }

  const isBusy = pendingAction !== null
  const symbol = currencySymbol(currency)

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    // Save is disabled for all of these, and a form whose submit button is
    // disabled ignores Enter, so this only narrows the types.
    if (!cardSaved || thresholdMicros === null || amountMicros === null || invalidReason) {
      console.debug('AutoRechargeModal ignored a submit it cannot send', { threshold, amount, invalidReason })
      return
    }

    setPendingAction('save')
    setErrorMessage(null)
    try {
      await api.setAutoRecharge(props.organizationId, {
        thresholdMicros,
        amountMicros,
      })
      await props.onChanged()
      props.onClose()
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setPendingAction(null)
    }
  }

  async function onTurnOff() {
    setPendingAction('turnOff')
    setErrorMessage(null)
    try {
      await api.disableAutoRecharge(props.organizationId)
      await props.onChanged()
      props.onClose()
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setPendingAction(null)
    }
  }

  return <Modal
    isOpen={props.isOpen}
    onClose={props.onClose}
  >
    <ModalContent>
      {/* One form around the footer too, so Save is the form's own submit
          button and Enter in either field presses it. */}
      <form onSubmit={onSubmit}>
        <ModalHeader>{
            t('billing.wallet.autoRechargeForm.title')
          }</ModalHeader>
        <ModalBody>
          <Input
            ref={thresholdRef}
            id='auto-recharge-threshold'
            type='number'
            inputMode='decimal'
            autoComplete='off'
            label={t('billing.wallet.autoRechargeForm.threshold')}
            // No `min`, and any step: the browser's own validation would paint
            // the field red and block Enter on its own terms, and the rules
            // this form checks are the server's, explained on the button.
            step='any'
            isDisabled={!cardSaved || isBusy}
            startContent={
              <span className='pointer-events-none text-small opacity-70'>{
                  symbol
                }</span>
            }
            value={threshold}
            onChange={(event) => setThreshold(event.currentTarget.value)}
          />
          <Input
            id='auto-recharge-amount'
            type='number'
            inputMode='decimal'
            autoComplete='off'
            label={t('billing.wallet.autoRechargeForm.amount')}
            step='any'
            isDisabled={!cardSaved || isBusy}
            startContent={
              <span className='pointer-events-none text-small opacity-70'>{
                  symbol
                }</span>
            }
            value={amount}
            onChange={(event) => setAmount(event.currentTarget.value)}
          />
          { cardSaved
            ? null
            : <p className='text-small opacity-80'>{
                t('billing.wallet.autoRechargeForm.needsCard')
              }</p>
          }
          { errorMessage
            ? <p role='alert' className='text-danger'>{
                errorMessage
              }</p>
            : null
          }
        </ModalBody>
        <ModalFooter>
          { autoRecharge
            ? <Button
                variant='light'
                className='mr-auto'
                isDisabled={isBusy}
                isLoading={pendingAction === 'turnOff'}
                onPress={onTurnOff}
              >
                <span>{
                    t('billing.wallet.autoRechargeForm.turnOff')
                  }</span>
              </Button>
            : null
          }
          <Button
            variant='flat'
            isDisabled={isBusy}
            onPress={props.onClose}
          >
            <span>{
                t('common.cancel')
              }</span>
          </Button>
          {/* Without a card the line beneath the fields already says why, so
              the tooltip speaks only to what was typed. */}
          <Tooltip
            content={invalidReason}
            isDisabled={!cardSaved || !invalidReason}
            placement='top'
          >
            <div>
              <Button
                type='submit'
                color='primary'
                isDisabled={!cardSaved || Boolean(invalidReason) || isBusy}
                isLoading={pendingAction === 'save'}
              >
                <span>{
                    t('common.save')
                  }</span>
              </Button>
            </div>
          </Tooltip>
        </ModalFooter>
      </form>
    </ModalContent>
  </Modal>
}
