// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  useAnubisApi,
  getApiErrorMessage,
  isPasskeySupported,
  serializeAuthenticationCredential,
  toCredentialRequestOptions,
} from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'

type Props = {
  /** Called once a session exists; the guest guard does the redirecting. */
  onSignedIn: () => Promise<void>
}

/**
 * Username-less passkey sign-in.
 *
 * The ceremony is discoverable, so there is nothing to type: the authenticator
 * names the account it holds a credential for, and the server resolves it. A
 * passkey is possession plus verification, so this path never meets the
 * second-factor challenge.
 */
export function PasskeySignInButton(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ isRunning, setIsRunning ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  if (!isPasskeySupported()) {
    console.debug('this browser has no WebAuthn support, hiding the passkey button')
    return null
  }

  async function onSignIn() {
    setIsRunning(true)
    setErrorMessage(null)
    try {
      const challenge = await api.startPasskeyLogin()
      const credential = await navigator.credentials.get(
        toCredentialRequestOptions(challenge.requestOptions),
      )
      await api.finishPasskeyLogin(
        challenge.stateToken,
        serializeAuthenticationCredential(credential),
      )
      await props.onSignedIn()
    }
    catch (error) {
      console.debug('passkey sign-in failed', error)
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('auth.passkey.failed'))
    }
    finally {
      setIsRunning(false)
    }
  }

  return <div className='relaxed'>
    <Button
      variant='bordered'
      className='w-full'
      isDisabled={isRunning}
      isLoading={isRunning}
      onPress={onSignIn}
    >
      <span>{
          t('auth.passkey.action')
        }</span>
    </Button>
    { errorMessage
      ? <p className='mt-2 text-danger'>{
          errorMessage
        }</p>
      : null
    }
  </div>
}
