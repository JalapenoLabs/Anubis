// Copyright © 2026 Jalapeno Labs

import type { AnubisApi, User } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'

// Misc
import { UrlTree } from '../../urls'

type ConfirmState = 'confirming' | 'confirmed' | 'failed'

// Confirmation tokens are single-use, so confirm each token exactly once per
// page load. Without this, StrictMode's development double-mount consumes the
// token on the first run and the second run reports a false failure.
const confirmationsByToken = new Map<string, Promise<User>>()

function confirmTokenOnce(api: AnubisApi, token: string): Promise<User> {
  let confirmation = confirmationsByToken.get(token)
  if (!confirmation) {
    confirmation = api.confirmEmailChange(token)
    confirmationsByToken.set(token, confirmation)
  }
  return confirmation
}

/**
 * Where the link mailed to a new address lands.
 *
 * The route is deliberately ungated: the link is opened from the new inbox,
 * which is often a browser with no session, and the token alone is what the
 * backend asks for.
 */
export function ConfirmEmailChangePage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()
  const [ searchParams ] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  const [ state, setState ] = useState<ConfirmState>('confirming')
  const [ confirmedEmail, setConfirmedEmail ] = useState('')

  useEffect(() => {
    let cancelled = false

    if (!token) {
      console.debug('the email-change page loaded without a token')
      setState('failed')
    }
    else {
      confirmTokenOnce(api, token)
        .then(async (user) => {
          await refresh()
          if (!cancelled) {
            setConfirmedEmail(user.email)
            setState('confirmed')
          }
        })
        .catch((error: unknown) => {
          console.debug('confirming the email change failed', error)
          if (!cancelled) {
            setState('failed')
          }
        })
    }

    return () => {
      cancelled = true
    }
  }, [])

  return <AuthLayout
    title={t('auth.changeEmail.title')}
    subtitle=''
  >
    { state === 'confirming'
      ? <div className='level'>
          <Spinner size='sm' />
          <p className='w-full'>{
              t('auth.changeEmail.confirming')
            }</p>
        </div>
      : null
    }
    { state === 'confirmed'
      ? <p className='relaxed'>{
          t('auth.changeEmail.confirmed', { email: confirmedEmail })
        }</p>
      : null
    }
    { state === 'failed'
      ? <p className='relaxed text-danger'>{
          t('auth.changeEmail.failed')
        }</p>
      : null
    }
    <div className='level-right mt-4 text-sm'>
      <Link to={UrlTree.root} className='text-primary'>{
          t('auth.changeEmail.goToDashboard')
        }</Link>
    </div>
  </AuthLayout>
}
