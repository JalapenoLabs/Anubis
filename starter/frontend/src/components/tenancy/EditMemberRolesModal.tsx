// Copyright © 2026 Jalapeno Labs

import type { GrantTier, RoleCatalog } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'
import { useRoleLabels } from '../roles/useRoleLabels'

// UI
import {
  Button,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
  Spinner,
} from '@heroui/react'
import { MemberRolesPicker } from '@jalapenolabs/anubis'

type Props = {
  /** Who is being edited, or null when nobody is. */
  email: string | null
  /** The roles they hold here now, by id. */
  heldRoleIds: string[]
  /** The organization's roles, with the viewer's standing where they are granted. */
  catalog: RoleCatalog | undefined
  /** Where the roles are granted. */
  tier: GrantTier
  onSave: (roleIds: string[]) => Promise<void>
  onClose: () => void
}

/**
 * Editing one member's roles: choose the set, save it, or change nothing.
 *
 * Mount it keyed by the member being edited: the choice starts from what they
 * hold, and a different member is a different dialog.
 *
 * The endpoint replaces roles wholesale, so the edit is a set rather than a
 * series of toggles, and one deliberate save is what makes the screen say the
 * same thing. It is also where a refusal belongs: taking the last
 * administrator's role answers `409` with a message worth reading, and the
 * dialog stays open holding both the message and the choice that earned it.
 */
export function EditMemberRolesModal(props: Props) {
  const { t } = useTranslation()
  const labels = useRoleLabels()
  const [ selected, setSelected ] = useState<string[]>(() => props.heldRoleIds)
  const [ isSaving, setIsSaving ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const isOpen = props.email !== null

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
    isOpen={isOpen}
    onClose={props.onClose}
  >
    <ModalContent>
      <ModalHeader>{
          t('team.members.rolesModalTitle', { email: props.email })
        }</ModalHeader>
      <ModalBody>
        { props.catalog
          ? <MemberRolesPicker
              roles={props.catalog.roles}
              viewer={props.catalog.viewer}
              tier={props.tier}
              held={props.heldRoleIds}
              value={selected}
              onChange={setSelected}
              isDisabled={isSaving}
              labels={labels.picker}
            />
          : <Spinner />
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
          isDisabled={isSaving}
          onPress={props.onClose}
        >
          <span>{
              t('common.cancel')
            }</span>
        </Button>
        <Button
          color='primary'
          isDisabled={isSaving || !props.catalog}
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
