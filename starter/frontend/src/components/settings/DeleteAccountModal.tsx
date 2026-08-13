// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'

// Core
import { useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
} from '@heroui/react'
import { PasswordField, TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

type DeleteAccountFormValues = {
  confirmation: string
  password: string
}

type Props = {
  user: User
  isOpen: boolean
  onClose: () => void
}

/**
 * Deletion behind two proofs: the account's own address, typed, and the password.
 *
 * Typing the address is the deliberate half. It cannot be clicked through, and
 * it names exactly which account is about to disappear, which a "type DELETE"
 * box does not.
 */
export function DeleteAccountModal(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()

  const email = props.user.email
  const resolver = useMemo(
    () => zodResolver(z.object({
      confirmation: z.string().refine((value) => value.trim().toLowerCase() === email.toLowerCase()),
      password: z.string().min(1),
    })),
    [ email ],
  )

  const form = useForm<DeleteAccountFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      confirmation: '',
      password: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      await api.deleteAccount(data.password)
      // The session cookie is gone; the auth guard takes it from here.
      await refresh()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <Modal
    isOpen={props.isOpen}
    onClose={props.onClose}
  >
    <ModalContent>
      <ModalHeader>{
          t('settings.security.danger.modalTitle')
        }</ModalHeader>
      <ModalBody>
        <p className='opacity-80'>{
            t('settings.security.danger.modalBody')
          }</p>
        <form onSubmit={onSubmit}>
          <TextField
            control={form.control}
            name='confirmation'
            label={t('settings.security.danger.confirmLabel', { email })}
            autoComplete='off'
            autoFocus
            isRequired
          />
          <PasswordField
            control={form.control}
            name='password'
            label={t('common.password')}
            autoComplete='current-password'
            isRequired
          />
          { form.formState.errors.root
            ? <p className='compact text-danger'>{
                form.formState.errors.root.message
              }</p>
            : null
          }
        </form>
      </ModalBody>
      <ModalFooter>
        <Button
          variant='flat'
          isDisabled={form.formState.isSubmitting}
          onPress={props.onClose}
        >
          <span>{
              t('common.cancel')
            }</span>
        </Button>
        <Button
          color='danger'
          isDisabled={!form.formState.isValid || form.formState.isSubmitting}
          isLoading={form.formState.isSubmitting}
          onPress={() => onSubmit()}
        >
          <span>{
              t('settings.security.danger.confirmAction')
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
