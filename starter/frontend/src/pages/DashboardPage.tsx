// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { AppShell } from '../components/AppShell'

// Misc
import { ANUBIS_VERSION } from '@jalapenolabs/anubis'

export function DashboardPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const [ verificationSent, setVerificationSent ] = useState(false)

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  async function onResendVerification() {
    try {
      await api.requestEmailVerification()
      setVerificationSent(true)
    }
    catch (error) {
      console.debug('re-sending the verification email failed', error)
    }
  }

  return <AppShell user={user}>
    { !user.emailVerified && !verificationSent
      ? <Card className='relaxed border-warning bg-warning-50'>
          <CardBody>
            <div className='level'>
              <p>{
                  t('dashboard.unverifiedBanner')
                }</p>
              <Button
                size='sm'
                color='warning'
                variant='flat'
                onPress={onResendVerification}
              >
                <span>{
                    t('dashboard.resendVerification')
                  }</span>
              </Button>
            </div>
          </CardBody>
        </Card>
      : null
    }
    { verificationSent
      ? <Card className='relaxed'>
          <CardBody>
            <p>{
                t('auth.verifyEmail.resent')
              }</p>
          </CardBody>
        </Card>
      : null
    }
    <div className='relaxed'>
      <h2 className='title'>{
          t('dashboard.welcomeTitle')
        }</h2>
      <p className='opacity-70'>{
          t('dashboard.welcomeBody')
        }</p>
    </div>
    <p className='text-sm opacity-50'>{
        t('app.frameworkVersion', { version: ANUBIS_VERSION })
      }</p>
  </AppShell>
}
