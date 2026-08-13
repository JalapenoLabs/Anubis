// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { PasswordField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const changePasswordSchema = z.object({
  currentPassword: z.string().min(1),
  newPassword: z.string().min(8),
})

type ChangePasswordFormValues = z.infer<typeof changePasswordSchema>
const resolver = zodResolver(changePasswordSchema)

/** Rotates the password. Every other signed-in browser is signed out. */
export function ChangePasswordCard() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ outcome, setOutcome ] = useState<string | null>(null)

  const form = useForm<ChangePasswordFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      currentPassword: '',
      newPassword: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    setOutcome(null)
    try {
      await api.changePassword({
        currentPassword: data.currentPassword,
        newPassword: data.newPassword,
      })
      form.reset({ currentPassword: '', newPassword: '' })
      setOutcome(t('settings.security.password.changed'))
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.security.password.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.password.subtitle')
        }</p>
      <form onSubmit={onSubmit}>
        <PasswordField
          control={form.control}
          name='currentPassword'
          label={t('settings.security.password.current')}
          autoComplete='current-password'
          isRequired
        />
        <PasswordField
          control={form.control}
          name='newPassword'
          label={t('settings.security.password.new')}
          help={t('auth.validation.passwordTooShort', { count: 8 })}
          autoComplete='new-password'
          isRequired
        />
        { form.formState.errors.root
          ? <p className='compact text-danger'>{
              form.formState.errors.root.message
            }</p>
          : null
        }
        { outcome
          ? <p className='compact opacity-80'>{
              outcome
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
                t('settings.security.password.action')
              }</span>
          </Button>
        </div>
      </form>
    </CardBody>
  </Card>
}
