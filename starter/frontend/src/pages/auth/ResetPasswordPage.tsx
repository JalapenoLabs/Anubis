// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Input } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'

// Misc
import { UrlTree } from '../../urls'

export function ResetPasswordPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ searchParams ] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  const [ password, setPassword ] = useState('')
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ outcome, setOutcome ] = useState<string | null>(null)
  const [ formError, setFormError ] = useState<string | null>(null)

  const passwordRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    passwordRef.current?.focus()
  }, [])

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setFormError(null)
    setIsSubmitting(true)
    try {
      const response = await api.confirmPasswordReset(token, password)
      setOutcome(response.message)
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setFormError(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsSubmitting(false)
    }
  }

  const isValid = password.length >= 8

  return <AuthLayout
    title={t('auth.resetPassword.title')}
    subtitle={t('auth.resetPassword.subtitle')}
  >
    { !token
      ? <p className='relaxed text-danger'>{
          t('auth.resetPassword.missingToken')
        }</p>
      : outcome
        ? <p className='relaxed'>{
            outcome
          }</p>
        : <form onSubmit={onSubmit}>
            <div className='compact'>
              <Input
                ref={passwordRef}
                type='password'
                label={t('auth.resetPassword.newPassword')}
                className='w-full'
                description={t('auth.validation.passwordTooShort', { count: 8 })}
                value={password}
                onChange={(event) => setPassword(event.currentTarget.value)}
              />
            </div>
            { formError
              ? <p className='compact text-danger'>{
                  formError
                }</p>
              : null
            }
            <div className='relaxed mt-6'>
              <Button
                type='submit'
                color='primary'
                className='w-full'
                isDisabled={!isValid || isSubmitting}
                isLoading={isSubmitting}
              >
                <span>{
                    t('auth.resetPassword.action')
                  }</span>
              </Button>
            </div>
          </form>
    }
    <div className='level-right mt-4 text-sm'>
      <Link to={UrlTree.signIn} className='text-primary'>{
          t('auth.forgotPassword.backToSignIn')
        }</Link>
    </div>
  </AuthLayout>
}
