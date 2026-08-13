// Copyright © 2026 Jalapeno Labs

import type { TeamRosterMember } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Checkbox,
  CheckboxGroup,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
} from '@heroui/react'

// Misc
import { ROLE_OPTIONS } from '../../permissions'

type Props = {
  /** The member being edited, or null when nothing is. */
  member: TeamRosterMember | null
  onSave: (roles: string[]) => Promise<void>
  onClose: () => void
}

/**
 * Editing one member's roles: choose the set, save it, or change nothing.
 *
 * The endpoint replaces roles wholesale, so the edit is a set rather than a
 * series of toggles, and one deliberate save is what makes the screen say the
 * same thing. It is also where the refusal belongs: demoting a team's last
 * admin answers `409` with a message worth reading, and the dialog stays open
 * holding both the message and the choice that earned it.
 */
export function EditMemberRolesModal(props: Props) {
  const { t } = useTranslation()
  const [ selected, setSelected ] = useState<string[]>([])
  const [ isSaving, setIsSaving ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const member = props.member
  useEffect(() => {
    setSelected(member?.roles ?? [])
    setErrorMessage(null)
  }, [ member ])

  async function onSave() {
    setIsSaving(true)
    setErrorMessage(null)
    try {
      await props.onSave(selected)
      props.onClose()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsSaving(false)
    }
  }

  return <Modal
    isOpen={Boolean(member)}
    onClose={props.onClose}
  >
    <ModalContent>
      <ModalHeader>{
          t('team.members.rolesModalTitle', { email: member?.email })
        }</ModalHeader>
      <ModalBody>
        <p className='opacity-80'>{
            t('team.members.rolesModalBody')
          }</p>
        <CheckboxGroup
          aria-label={t('team.members.rolesHeader')}
          value={selected}
          onValueChange={setSelected}
        >
          {
            // Role keys are the application's own vocabulary, from roles.yml,
            // and are shown as declared rather than translated.
            ROLE_OPTIONS.map((role) => (
              <Checkbox key={role} value={role}>{
                  role
                }</Checkbox>
            ))
          }
        </CheckboxGroup>
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
          isDisabled={isSaving}
          onPress={props.onClose}
        >
          <span>{
              t('common.cancel')
            }</span>
        </Button>
        <Button
          color='primary'
          isDisabled={isSaving}
          isLoading={isSaving}
          onPress={onSave}
        >
          <span>{
              t('common.save')
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
