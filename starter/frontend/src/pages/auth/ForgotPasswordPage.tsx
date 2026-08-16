// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, useRetryCountdown, getRetryAfterSeconds } from '@jalapenolabs/anubis'

// UI
import { Button, Input } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'
import { RateLimitNotice } from '../../components/RateLimitNotice'

// Misc
import { DESTINATION_PARAM, UrlTree, getUrlWithDestination } from '../../urls'

export function ForgotPasswordPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const retry = useRetryCountdown()
  const [ searchParams ] = useSearchParams()
  const [ email, setEmail ] = useState('')
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ sentMessage, setSentMessage ] = useState<string | null>(null)

  const emailRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    emailRef.current?.focus()
  }, [])

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setIsSubmitting(true)
    try {
      const response = await api.requestPasswordReset(email.trim())
      setSentMessage(response.message)
    }
    catch (error) {
      // A rate-limited refusal keeps the form on screen with a live countdown:
      // the address is fine, the timing is not.
      const wait = getRetryAfterSeconds(error)
      if (wait) {
        retry.start(wait)
        return
      }

      console.debug('password reset request failed', error)
      setSentMessage(t('common.somethingWentWrong'))
    }
    finally {
      setIsSubmitting(false)
    }
  }

  return <AuthLayout
    title={t('auth.forgotPassword.title')}
    subtitle={t('auth.forgotPassword.subtitle')}
  >
    { sentMessage
      ? <p className='relaxed'>{
          sentMessage
        }</p>
      : <form onSubmit={onSubmit}>
          <div className='compact'>
            <Input
              ref={emailRef}
              type='email'
              label={t('common.email')}
              className='w-full'
              value={email}
              onChange={(event) => setEmail(event.currentTarget.value)}
            />
          </div>
          <RateLimitNotice remaining={retry.remaining} />
          <div className='relaxed mt-6'>
            <Button
              type='submit'
              color='primary'
              className='w-full'
              isDisabled={!email.trim() || isSubmitting || !retry.done}
              isLoading={isSubmitting}
            >
              <span>{
                  t('auth.forgotPassword.action')
                }</span>
            </Button>
          </div>
        </form>
    }
    <div className='level-right mt-4 text-sm'>
      <Link
        to={getUrlWithDestination(UrlTree.signIn, searchParams.get(DESTINATION_PARAM))}
        className='text-primary'
      >{
          t('auth.forgotPassword.backToSignIn')
        }</Link>
    </div>
  </AuthLayout>
}
