// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { useAnubisApi } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody, Chip, Spinner } from '@heroui/react'
import { PasswordPromptForm } from './PasswordPromptForm'
import { TotpEnrollmentModal } from './TotpEnrollmentModal'

/** Two-factor status, enrollment, and the password-confirmed way back out. */
export function TwoFactorCard() {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const status = useSWR('auth/mfa', () => api.getMfaStatus())
  const [ isEnrolling, setIsEnrolling ] = useState(false)
  const [ isDisabling, setIsDisabling ] = useState(false)

  const isEnabled = status.data?.totpEnabled ?? false

  async function onEnrollmentClosed(didEnroll: boolean) {
    setIsEnrolling(false)
    if (didEnroll) {
      await status.mutate()
    }
  }

  async function onDisable(password: string) {
    await api.disableTotp(password)
    setIsDisabling(false)
    await status.mutate()
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <div className='level'>
        <h3 className='text-xl font-semibold'>{
            t('settings.security.twoFactor.title')
          }</h3>
        { status.isLoading
          ? <Spinner size='sm' />
          : <Chip
              size='sm'
              variant='flat'
              color={isEnabled ? 'success' : 'default'}
            >{
              isEnabled
                ? t('settings.security.twoFactor.enabled')
                : t('settings.security.twoFactor.disabled')
            }</Chip>
        }
      </div>
      <p className='compact opacity-70'>{
          t('settings.security.twoFactor.subtitle')
        }</p>
      { isDisabling
        ? <PasswordPromptForm
            submitLabel={t('settings.security.twoFactor.disableAction')}
            submitColor='danger'
            onCancel={() => setIsDisabling(false)}
            onConfirm={onDisable}
          />
        : <div className='level-right'>
            { isEnabled
              ? <Button
                  color='danger'
                  variant='flat'
                  isDisabled={status.isLoading}
                  onPress={() => setIsDisabling(true)}
                >
                  <span>{
                      t('settings.security.twoFactor.disableAction')
                    }</span>
                </Button>
              : <Button
                  color='primary'
                  isDisabled={status.isLoading}
                  onPress={() => setIsEnrolling(true)}
                >
                  <span>{
                      t('settings.security.twoFactor.enableAction')
                    }</span>
                </Button>
            }
          </div>
      }
      <TotpEnrollmentModal
        isOpen={isEnrolling}
        onClose={onEnrollmentClosed}
      />
    </CardBody>
  </Card>
}
