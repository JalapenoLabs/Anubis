// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { EmailField, PasswordField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const changeEmailSchema = z.object({
  newEmail: z.email(),
  password: z.string().min(1),
})

type ChangeEmailFormValues = z.infer<typeof changeEmailSchema>
const resolver = zodResolver(changeEmailSchema)

type Props = {
  user: User
}

/**
 * Moves the account to a new address, once its owner proves they read it.
 *
 * Nothing changes here: the request only mails a link, and confirming that
 * link is what swaps the address. That is why the screen ends by pointing at
 * the new inbox rather than by reporting success.
 */
export function ChangeEmailCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ sentToAddress, setSentToAddress ] = useState<string | null>(null)

  const form = useForm<ChangeEmailFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      newEmail: '',
      password: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    setSentToAddress(null)
    try {
      await api.requestEmailChange({
        newEmail: data.newEmail.trim(),
        password: data.password,
      })
      setSentToAddress(data.newEmail.trim())
      form.reset({ newEmail: '', password: '' })
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.security.email.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.email.current', { email: props.user.email })
        }</p>
      <form onSubmit={onSubmit}>
        <EmailField
          control={form.control}
          name='newEmail'
          label={t('settings.security.email.newAddress')}
          help={t('settings.security.email.help')}
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
        { sentToAddress
          ? <p className='compact opacity-80'>{
              t('settings.security.email.sent', { email: sentToAddress })
            }</p>
          : null
        }
        <div className='level-right mt-4'>
          <Button
            type='submit'
            color='primary'
            isDisabled={!form.formState.isValid || form.formState.isSubmitting}
            isLoading={form.formState.isSubmitting}
          >
            <span>{
                t('settings.security.email.action')
              }</span>
          </Button>
        </div>
      </form>
    </CardBody>
  </Card>
}
