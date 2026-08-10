// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Input, Tooltip } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import { DESTINATION_PARAM, UrlTree, getUrlWithDestination } from '../../urls'

const signInSchema = z.object({
  email: z.email(),
  password: z.string().min(8),
})

type SignInFormValues = z.infer<typeof signInSchema>
const resolver = zodResolver(signInSchema)

export function SignInPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()
  const [ searchParams ] = useSearchParams()
  const [ formError, setFormError ] = useState<string | null>(null)

  // The page the guard sent the user away from, kept on the links out of here
  // so a detour through sign-up or a password reset does not lose it.
  const destination = searchParams.get(DESTINATION_PARAM)

  const emailRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    emailRef.current?.focus()
  }, [])

  const form = useForm<SignInFormValues>({
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
      await api.login(data)
      // RequireGuest redirects to the preserved destination, or to the
      // dashboard, once the user refreshes in.
      await refresh()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setFormError(message ?? t('common.somethingWentWrong'))
    }
  })

  const isValid = form.formState.isValid

  return <AuthLayout
    title={t('auth.signIn.title')}
    subtitle={t('auth.signIn.subtitle')}
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
                t('auth.signIn.action')
              }</span>
          </Button>
        </div>
      </Tooltip>
    </form>
    <div className='level mt-4 text-sm'>
      <Link
        to={getUrlWithDestination(UrlTree.forgotPassword, destination)}
        className='opacity-70 hover:opacity-100'
      >{
          t('auth.signIn.forgotPassword')
        }</Link>
      <span>
        <span className='opacity-70'>{
            t('auth.signIn.noAccount')
          }</span>
        {' '}
        <Link to={getUrlWithDestination(UrlTree.signUp, destination)} className='text-primary'>{
            t('auth.signIn.goToSignUp')
          }</Link>
      </span>
    </div>
  </AuthLayout>
}
