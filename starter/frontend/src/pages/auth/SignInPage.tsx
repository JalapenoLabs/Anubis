// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'
import { PasskeySignInButton } from './PasskeySignInButton'
import { SignInEmailCodeForm } from './SignInEmailCodeForm'
import { SignInMfaForm } from './SignInMfaForm'
import { SignInPasswordForm } from './SignInPasswordForm'

// Misc
import {
  AUTH_ERROR_PARAM,
  DESTINATION_PARAM,
  UrlTree,
  getUrlWithDestination,
  // 🐺 anubis:oauth-imports
} from '../../urls'

/**
 * The codes an OAuth sign-in fails back with, and the string each one renders.
 *
 * The codes are the framework's (`anubis::auth::oauth`), so they are wire
 * values rather than translation keys. A code with no entry here falls back to
 * the generic message, which is what keeps an unrecognized one off the screen.
 */
const oauthErrorKeys: Record<string, string | undefined> = {
  oauth_unavailable: 'auth.oauth.errors.unavailable',
  oauth_denied: 'auth.oauth.errors.denied',
  oauth_expired: 'auth.oauth.errors.expired',
  oauth_email_unavailable: 'auth.oauth.errors.emailUnavailable',
  oauth_email_unverified: 'auth.oauth.errors.emailUnverified',
  oauth_failed: 'auth.oauth.errors.failed',
}

/**
 * Which sign-in step is on screen.
 *
 * Every method ends in one of two places: a session, or the second-factor
 * challenge. Modeling the challenge as a step of this page, rather than as a
 * route of its own, is what keeps the preserved destination in the URL through
 * the whole flow.
 */
type SignInStep =
  | { name: 'password' }
  | { name: 'email-code' }
  | { name: 'mfa', mfaToken: string }

export function SignInPage() {
  const { t } = useTranslation()
  const { refresh } = useCurrentUser()
  const [ searchParams ] = useSearchParams()
  const [ step, setStep ] = useState<SignInStep>({ name: 'password' })

  // The page the guard sent the user away from, kept on the links out of here
  // so a detour through sign-up or a password reset does not lose it.
  const destination = searchParams.get(DESTINATION_PARAM)

  // An OAuth flow that failed lands back here carrying its reason.
  const oauthError = searchParams.get(AUTH_ERROR_PARAM)

  async function onSignedIn() {
    // RequireGuest redirects to the preserved destination, or to the
    // dashboard, once the user refreshes in.
    await refresh()
  }

  function onMfaRequired(mfaToken: string) {
    setStep({ name: 'mfa', mfaToken })
  }

  if (step.name === 'mfa') {
    return <AuthLayout
      title={t('auth.mfa.title')}
      subtitle={t('auth.mfa.subtitle')}
    >
      <SignInMfaForm
        mfaToken={step.mfaToken}
        onSignedIn={onSignedIn}
        onCancel={() => setStep({ name: 'password' })}
      />
    </AuthLayout>
  }

  return <AuthLayout
    title={t('auth.signIn.title')}
    subtitle={t('auth.signIn.subtitle')}
  >
    { oauthError
      ? <p className='relaxed text-danger'>{
          t(oauthErrorKeys[oauthError] ?? 'common.somethingWentWrong')
        }</p>
      : null
    }
    { step.name === 'password'
      ? <SignInPasswordForm
          onSignedIn={onSignedIn}
          onMfaRequired={onMfaRequired}
        />
      : <SignInEmailCodeForm
          onSignedIn={onSignedIn}
          onMfaRequired={onMfaRequired}
        />
    }
    <Button
      variant='light'
      className='mb-2 w-full'
      onPress={() => setStep(
        step.name === 'password'
          ? { name: 'email-code' }
          : { name: 'password' },
      )}
    >
      <span>{
          step.name === 'password'
            ? t('auth.emailCode.switchTo')
            : t('auth.emailCode.switchBack')
        }</span>
    </Button>
    <PasskeySignInButton onSignedIn={onSignedIn} />
    {/* One button per provider, written by `anubis scaffold oauth <provider>`. */}
    {/* 🐺 anubis:oauth-providers */}
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
