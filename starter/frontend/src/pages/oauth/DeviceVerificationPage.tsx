// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'

// UI
import { AuthLayout } from '../../components/AuthLayout'
import { UserCodeForm } from '../../components/oauth/UserCodeForm'

// Misc
import { DEVICE_RESULT_PARAM, DEVICE_USER_CODE_PARAM, UrlTree } from '../../urls'

/** What the page says once the person decided, keyed by the backend's result. */
const resultCopy = {
  approved: {
    title: 'oauth.device.approved.title',
    body: 'oauth.device.approved.body',
  },
  denied: {
    title: 'oauth.device.denied.title',
    body: 'oauth.device.denied.body',
  },
} as const

/**
 * Where a person connects a device that has no browser of its own.
 *
 * The device shows a short code and this page's address; the person types the
 * code here and approves on the consent screen every connecting program uses.
 * The backend returns the browser here afterwards with how it went. A link the
 * device offers carries its code, which prefills the field but is never sent
 * on the person's behalf: someone else's device can send a link too.
 */
export function DeviceVerificationPage() {
  const { t } = useTranslation()
  const [ searchParams ] = useSearchParams()

  const result = searchParams.get(DEVICE_RESULT_PARAM)
  const copy = result === 'approved' || result === 'denied'
    ? resultCopy[result]
    : null

  if (copy) {
    return <AuthLayout
      title={t(copy.title)}
      subtitle=''
    >
      <p className='relaxed'>{
          t(copy.body)
        }</p>
      <Link to={UrlTree.deviceVerification} className='text-primary'>{
          t('oauth.device.another')
        }</Link>
    </AuthLayout>
  }

  return <AuthLayout
    title={t('oauth.device.title')}
    subtitle={t('oauth.device.subtitle')}
  >
    <UserCodeForm initialCode={searchParams.get(DEVICE_USER_CODE_PARAM) ?? ''} />
  </AuthLayout>
}
