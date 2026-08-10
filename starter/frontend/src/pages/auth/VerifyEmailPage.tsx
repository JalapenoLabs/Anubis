// Copyright © 2026 Jalapeno Labs

import type { AnubisApi } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Button, Spinner } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'

// Misc
import { UrlTree } from '../../urls'

type VerifyState = 'verifying' | 'verified' | 'failed'

// Verification tokens are single-use, so confirm each token exactly once per
// page load. Without this, StrictMode's development double-mount consumes the
// token on the first run and the second run reports a false failure.
const confirmationsByToken = new Map<string, Promise<unknown>>()

function confirmTokenOnce(api: AnubisApi, token: string): Promise<unknown> {
  let confirmation = confirmationsByToken.get(token)
  if (!confirmation) {
    confirmation = api.confirmEmailVerification(token)
    confirmationsByToken.set(token, confirmation)
  }
  return confirmation
}

export function VerifyEmailPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user, refresh } = useCurrentUser()
  const [ searchParams ] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  const [ state, setState ] = useState<VerifyState>('verifying')
  const [ resent, setResent ] = useState(false)

  useEffect(() => {
    let cancelled = false

    if (!token) {
      setState('failed')
    }
    else {
      confirmTokenOnce(api, token)
        .then(async () => {
          await refresh()
          if (!cancelled) {
            setState('verified')
          }
        })
        .catch(async (error: unknown) => {
          console.debug('email verification failed', error)
          // A dead link on an already-verified account is a success, not a
          // scare: old links get clicked twice.
          const refreshed = await refresh()
          if (!cancelled) {
            setState(refreshed?.emailVerified ? 'verified' : 'failed')
          }
        })
    }

    return () => {
      cancelled = true
    }
  }, [])

  async function onResend() {
    try {
      await api.requestEmailVerification()
      setResent(true)
    }
    catch (error) {
      console.debug('re-sending the verification email failed', error)
    }
  }

  return <AuthLayout
    title={t('auth.verifyEmail.title')}
    subtitle=''
  >
    { state === 'verifying'
      ? <div className='level'>
          <Spinner size='sm' />
          <p className='w-full'>{
              t('auth.verifyEmail.verifying')
            }</p>
        </div>
      : null
    }
    { state === 'verified'
      ? <p className='relaxed'>{
          t('auth.verifyEmail.verified')
        }</p>
      : null
    }
    { state === 'failed'
      ? <div>
          <p className='relaxed text-danger'>{
              t('auth.verifyEmail.failed')
            }</p>
          { user && !resent
            ? <Button
                color='primary'
                variant='flat'
                className='relaxed w-full'
                onPress={onResend}
              >
                <span>{
                    t('auth.verifyEmail.resend')
                  }</span>
              </Button>
            : null
          }
          { resent
            ? <p className='relaxed'>{
                t('auth.verifyEmail.resent')
              }</p>
            : null
          }
        </div>
      : null
    }
    <div className='level-right mt-4 text-sm'>
      <Link to={UrlTree.root} className='text-primary'>{
          t('auth.verifyEmail.goToDashboard')
        }</Link>
    </div>
  </AuthLayout>
}
