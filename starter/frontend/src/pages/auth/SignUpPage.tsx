// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Input, Tooltip } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import { UrlTree } from '../../urls'

const signUpSchema = z.object({
  email: z.email(),
  password: z.string().min(8),
})

type SignUpFormValues = z.infer<typeof signUpSchema>
const resolver = zodResolver(signUpSchema)

export function SignUpPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()
  const [ formError, setFormError ] = useState<string | null>(null)

  const emailRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    emailRef.current?.focus()
  }, [])

  const form = useForm<SignUpFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      email: '',
      password: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    setFormError(null)
    try {
      await api.register(data)
      // RequireGuest redirects to the dashboard once the user refreshes in.
      await refresh()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setFormError(message ?? t('common.somethingWentWrong'))
    }
  })

  const isValid = form.formState.isValid

  return <AuthLayout
    title={t('auth.signUp.title')}
    subtitle={t('auth.signUp.subtitle')}
  >
    <form onSubmit={onSubmit}>
      <div className='compact'>
        <Input
          ref={emailRef}
          type='email'
          label={t('common.email')}
          className='w-full'
          isInvalid={Boolean(form.formState.errors.email && form.formState.touchedFields.email)}
          value={form.watch('email')}
          onChange={(event) => {
            form.setValue('email', event.currentTarget.value, {
              shouldDirty: true,
              shouldValidate: true,
              shouldTouch: true,
            })
          }}
        />
      </div>
      <div className='compact'>
        <Input
          type='password'
          label={t('common.password')}
          className='w-full'
          description={t('auth.validation.passwordTooShort', { count: 8 })}
          value={form.watch('password')}
          onChange={(event) => {
            form.setValue('password', event.currentTarget.value, {
              shouldDirty: true,
              shouldValidate: true,
            })
          }}
        />
      </div>
      { formError
        ? <p className='compact text-danger'>{
            formError
          }</p>
        : null
      }
      <Tooltip
        content={t('auth.validation.fillAllFields')}
        isDisabled={isValid}
        placement='top'
      >
        <div className='relaxed mt-6'>
          <Button
            type='submit'
            color='primary'
            className='w-full'
            isDisabled={!isValid || form.formState.isSubmitting}
            isLoading={form.formState.isSubmitting}
          >
            <span>{
                t('auth.signUp.action')
              }</span>
          </Button>
        </div>
      </Tooltip>
    </form>
    <div className='level-right mt-4 text-sm'>
      <span>
        <span className='opacity-70'>{
            t('auth.signUp.haveAccount')
          }</span>
        {' '}
        <Link to={UrlTree.signIn} className='text-primary'>{
            t('auth.signUp.goToSignIn')
          }</Link>
      </span>
    </div>
  </AuthLayout>
}
