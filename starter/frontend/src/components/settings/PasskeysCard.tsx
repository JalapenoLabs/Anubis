// Copyright © 2026 Jalapeno Labs

import type { Passkey } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import {
  useAnubisApi,
  getApiErrorMessage,
  isPasskeySupported,
  serializeRegistrationCredential,
  toCredentialCreationOptions,
} from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Card,
  CardBody,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'
import { TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const addPasskeySchema = z.object({
  name: z.string().trim().min(1),
})

type AddPasskeyFormValues = z.infer<typeof addPasskeySchema>
const resolver = zodResolver(addPasskeySchema)

/** Registered passkeys: the list, the registration ceremony, and removal. */
export function PasskeysCard() {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const passkeys = useSWR('auth/passkeys', () => api.listPasskeys())
  const [ removingId, setRemovingId ] = useState<string | null>(null)
  const isSupported = isPasskeySupported()

  const form = useForm<AddPasskeyFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      name: '',
    },
  })

  const onAdd = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      const challenge = await api.startPasskeyRegistration()
      const credential = await navigator.credentials.create(
        toCredentialCreationOptions(challenge.creationOptions),
      )
      await api.finishPasskeyRegistration(
        challenge.stateToken,
        serializeRegistrationCredential(credential),
        data.name.trim(),
      )
      form.reset({ name: '' })
      await passkeys.mutate()
    }
    catch (error) {
      console.debug('registering a passkey failed', error)
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('settings.security.passkeys.ceremonyFailed') })
    }
  })

  async function onRemove(passkeyId: string) {
    setRemovingId(passkeyId)
    try {
      await api.deletePasskey(passkeyId)
      await passkeys.mutate()
    }
    catch (error) {
      console.debug('removing a passkey failed', error)
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
    finally {
      setRemovingId(null)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.security.passkeys.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.passkeys.subtitle')
        }</p>
      <div className='relaxed'>
        <Table
          removeWrapper
          aria-label={t('settings.security.passkeys.title')}
        >
          <TableHeader>
            <TableColumn>{
                t('settings.security.passkeys.name')
              }</TableColumn>
            <TableColumn>{
                t('settings.security.passkeys.added')
              }</TableColumn>
            <TableColumn>{
                t('settings.security.passkeys.lastUsed')
              }</TableColumn>
            <TableColumn>{
                t('common.actions')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={passkeys.data ?? []}
            isLoading={passkeys.isLoading}
            emptyContent={t('settings.security.passkeys.empty')}
          >
            {
              (passkey: Passkey) => (
                <TableRow key={passkey.id}>
                  <TableCell>{
                      passkey.name
                    }</TableCell>
                  <TableCell>{
                      new Date(passkey.createdAt).toLocaleDateString()
                    }</TableCell>
                  <TableCell>{
                      passkey.lastUsedAt
                        ? new Date(passkey.lastUsedAt).toLocaleDateString()
                        : t('settings.security.passkeys.neverUsed')
                    }</TableCell>
                  <TableCell>
                    <Button
                      size='sm'
                      variant='light'
                      color='danger'
                      isDisabled={Boolean(removingId)}
                      isLoading={removingId === passkey.id}
                      onPress={() => onRemove(passkey.id)}
                    >
                      <span>{
                          t('common.delete')
                        }</span>
                    </Button>
                  </TableCell>
                </TableRow>
              )
            }
          </TableBody>
        </Table>
      </div>
      { isSupported
        ? <form onSubmit={onAdd}>
            <div className='level items-start'>
              <TextField
                control={form.control}
                name='name'
                label={t('settings.security.passkeys.nameLabel')}
                help={t('settings.security.passkeys.nameHelp')}
                className='w-full'
              />
              <Button
                type='submit'
                color='primary'
                className='mt-1 shrink-0'
                isDisabled={!form.formState.isValid || form.formState.isSubmitting}
                isLoading={form.formState.isSubmitting}
              >
                <span>{
                    t('settings.security.passkeys.addAction')
                  }</span>
              </Button>
            </div>
          </form>
        : <p className='opacity-70'>{
            t('settings.security.passkeys.unsupported')
          }</p>
      }
      { form.formState.errors.root
        ? <p className='mt-2 text-danger'>{
            form.formState.errors.root.message
          }</p>
        : null
      }
    </CardBody>
  </Card>
}
