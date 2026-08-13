// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
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
} from '@heroui/react'
import { useTeamContext } from '../../context/TeamProvider'

// Misc
import { getOrganizationSettingsUrl } from '../../urls'

type Props = {
  isOpen: boolean
  onClose: () => void
}

/**
 * Creates an organization, and moves the user into it.
 *
 * The server gives every organization a default team and makes the creator an
 * admin of both, so the one field here is all a new tenant needs. Selecting
 * that team is what makes the creation visible everywhere else: the switcher,
 * the roster, and every team-scoped screen read the same selection.
 */
export function CreateOrganizationModal(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const { memberships, selectTeam } = useTeamContext()

  const [ name, setName ] = useState('')
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  async function onCreate() {
    setIsSubmitting(true)
    setErrorMessage(null)
    try {
      const created = await api.createOrganization(name.trim())
      await memberships.refresh()
      selectTeam(created.team.id)
      setName('')
      props.onClose()
      navigate(getOrganizationSettingsUrl(created.organization.id))
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
          t('organization.create.title')
        }</ModalHeader>
      <ModalBody>
        <p className='opacity-80'>{
            t('organization.create.subtitle')
          }</p>
        <Input
          autoFocus
          label={t('organization.create.nameLabel')}
          value={name}
          onChange={(event) => setName(event.currentTarget.value)}
        />
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
          color='primary'
          isDisabled={!name.trim() || isSubmitting}
          isLoading={isSubmitting}
          onPress={onCreate}
        >
          <span>{
              t('organization.create.action')
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
