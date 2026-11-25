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
  Switch,
  Tooltip,
} from '@heroui/react'

// Misc
import {
  MICROS_PER_UNIT,
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
}

/** The sentence a disabled Continue button explains itself with, per problem. */
const reasonByProblem = {
  missing: 'billing.wallet.topUp.amountMissing',
  belowMinimum: 'billing.wallet.topUp.amountBelowMinimum',
  partialUnit: 'billing.wallet.partialUnit',
} as const satisfies Record<AmountProblem, string>

/**
 * Buys credit: an amount, whether to keep the card, and off to Stripe Checkout.
 *
 * Nothing is credited here. The browser leaves for Stripe and comes back to the
 * billing screen with `?top_up=`, and the credit lands when Stripe's payment
 * event does. Saving the card is what lets auto-recharge charge it later
 * without the person present, which is why it is a choice made at payment.
 */
export function TopUpModal(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { currency, minimumTopUpMicros, chargeStepMicros: stepMicros } = props.wallet

  const [ amount, setAmount ] = useState('')
  const [ saveCard, setSaveCard ] = useState(false)
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  // The modal stays mounted while closed, so each opening starts the form over
  // from the minimum and puts the cursor in it, rather than only the first.
  const amountRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (!props.isOpen) {
      return
    }

    setAmount(String(minimumTopUpMicros / MICROS_PER_UNIT))
    setSaveCard(false)
    setErrorMessage(null)
    amountRef.current?.focus()
  }, [ props.isOpen ])

  const amountMicros = parseMicros(amount)
  const problem = findAmountProblem(amountMicros, minimumTopUpMicros, stepMicros)

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    // The button is disabled while the amount is unusable, and a form whose
    // submit button is disabled ignores Enter, so this only guards the types.
    if (amountMicros === null || problem) {
      console.debug('TopUpModal ignored a submit with an unusable amount', { amount, problem })
      return
    }

    setIsSubmitting(true)
    setErrorMessage(null)
    try {
      window.location.href = await api.startTopUp(props.organizationId, {
        amountMicros,
        saveCard,
      })
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
      setIsSubmitting(false)
    }
  }

  return <Modal
    isOpen={props.isOpen}
    onClose={props.onClose}
  >
    <ModalContent>
      {/* One form around the footer too, so the submit button is the form's
          own and Enter in the amount field presses it. */}
      <form onSubmit={onSubmit}>
        <ModalHeader>{
            t('billing.wallet.topUp.title')
          }</ModalHeader>
        <ModalBody>
          <Input
            ref={amountRef}
            id='top-up-amount'
            type='number'
            inputMode='decimal'
            autoComplete='off'
            label={t('billing.wallet.topUp.amount')}
            // No `min`, and any step: the browser's own validation would paint
            // the field red and block Enter on its own terms, and the rules
            // this form checks are the server's, explained on the button.
            step='any'
            startContent={
              <span className='pointer-events-none text-small opacity-70'>{
                  currencySymbol(currency)
                }</span>
            }
            value={amount}
            onChange={(event) => setAmount(event.currentTarget.value)}
          />
          <Switch
            id='top-up-save-card'
            isSelected={saveCard}
            onValueChange={setSaveCard}
          >{
              t('billing.wallet.topUp.saveCard')
            }</Switch>
          { errorMessage
            ? <p role='alert' className='text-danger'>{
                errorMessage
              }</p>
            : null
          }
        </ModalBody>
        <ModalFooter>
          <Button
            variant='flat'
            isDisabled={isSubmitting}
            onPress={props.onClose}
          >
            <span>{
                t('common.cancel')
              }</span>
          </Button>
          <Tooltip
            content={
              problem
                ? t(reasonByProblem[problem], {
                    minimum: formatMicros(minimumTopUpMicros, currency),
                    step: formatMicros(stepMicros, currency),
                  })
                : null
            }
            isDisabled={!problem}
            placement='top'
          >
            <div>
              <Button
                type='submit'
                color='primary'
                isDisabled={Boolean(problem) || isSubmitting}
                isLoading={isSubmitting}
              >
                <span>{
                    t('billing.wallet.topUp.submit')
                  }</span>
              </Button>
            </div>
          </Tooltip>
        </ModalFooter>
      </form>
    </ModalContent>
  </Modal>
}
