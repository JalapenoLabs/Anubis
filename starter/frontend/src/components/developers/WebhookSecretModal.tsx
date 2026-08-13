// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

// UI
import { Button, Modal, ModalBody, ModalContent, ModalFooter, ModalHeader } from '@heroui/react'

type Props = {
  /** The secret to show, or null when there is nothing to show. */
  secret: string | null
  onClose: () => void
}

/**
 * The one and only sighting of an endpoint's signing secret.
 *
 * The server keeps the secret sealed and never serves it again, so this is a
 * modal rather than a line in the table: closing it is the user saying they
 * have it, and the copy is the only chance they get.
 */
export function WebhookSecretModal(props: Props) {
  const { t } = useTranslation()
  const [ hasCopied, setHasCopied ] = useState(false)

  async function onCopy() {
    if (!props.secret) {
      console.debug('copy pressed with no secret to copy')
      return
    }

    try {
      await navigator.clipboard.writeText(props.secret)
      setHasCopied(true)
    }
    catch (error) {
      console.debug('copying the signing secret failed', error)
    }
  }

  return <Modal
    isOpen={Boolean(props.secret)}
    onClose={() => {
      setHasCopied(false)
      props.onClose()
    }}
  >
    <ModalContent>
      <ModalHeader>{
          t('developers.webhooks.secret.title')
        }</ModalHeader>
      <ModalBody>
        <p className='compact opacity-70'>{
            t('developers.webhooks.secret.subtitle')
          }</p>
        <code className='block rounded-medium bg-default-100 p-3 break-all'>{
            props.secret
          }</code>
      </ModalBody>
      <ModalFooter>
        <Button variant='light' onPress={onCopy}>
          <span>{
              hasCopied
                ? t('developers.webhooks.secret.copied')
                : t('developers.webhooks.secret.copy')
            }</span>
        </Button>
        <Button
          color='primary'
          onPress={() => {
            setHasCopied(false)
            props.onClose()
          }}
        >
          <span>{
              t('developers.webhooks.secret.acknowledge')
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
