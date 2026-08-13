// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Input,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
} from '@heroui/react'

type Props = {
  isOpen: boolean
  title: string
  body: string
  confirmLabel: string
  /**
   * The exact text the user must type before confirming.
   *
   * Set it for the actions nothing brings back, following the account deletion
   * modal: typing the tenant's name cannot be clicked through, and it names
   * exactly which tenant is about to disappear.
   */
  confirmationPhrase?: string
  confirmationLabel?: string
  onConfirm: () => Promise<void>
  onClose: () => void
}

/**
 * The confirmation every destructive tenancy action goes through.
 *
 * The refusals these actions can meet are the interesting part: leaving as a
 * team's last admin, or deleting a tenant an application still holds records
 * in, answer `409` with a message worth reading, so the modal stays open and
 * shows it instead of closing on a failure.
 */
export function ConfirmActionModal(props: Props) {
  const { t } = useTranslation()
  const [ typed, setTyped ] = useState('')
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  // Each opening is its own question: the refusal that stopped the last one is
  // not an answer to this one, and neither is what was typed for it.
  useEffect(() => {
    if (props.isOpen) {
      setTyped('')
      setErrorMessage(null)
    }
  }, [ props.isOpen ])

  const isConfirmed = !props.confirmationPhrase
    || typed.trim().toLowerCase() === props.confirmationPhrase.trim().toLowerCase()

  async function onConfirm() {
    setIsSubmitting(true)
    setErrorMessage(null)
    try {
      await props.onConfirm()
      setTyped('')
      props.onClose()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsSubmitting(false)
    }
  }

  return <Modal
    isOpen={props.isOpen}
    onClose={props.onClose}
  >
    <ModalContent>
      <ModalHeader>{
          props.title
        }</ModalHeader>
      <ModalBody>
        <p className='opacity-80'>{
            props.body
          }</p>
        { props.confirmationPhrase
          ? <Input
              autoFocus
              autoComplete='off'
              label={props.confirmationLabel}
              value={typed}
              onChange={(event) => setTyped(event.currentTarget.value)}
            />
          : null
        }
        { errorMessage
          ? <p className='compact text-danger'>{
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
        <Button
          color='danger'
          isDisabled={!isConfirmed || isSubmitting}
          isLoading={isSubmitting}
          onPress={onConfirm}
        >
          <span>{
              props.confirmLabel
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
