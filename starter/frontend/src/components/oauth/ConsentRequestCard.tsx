// Copyright © 2026 Jalapeno Labs

import type { AuthorizationRequest } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage, useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Alert, Button } from '@heroui/react'

// Misc
import { consentRefusalKey } from './consentRefusal'

type Props = {
  request: AuthorizationRequest
}

/**
 * What a connecting program asks for, and the person's two answers.
 *
 * The MCP specification requires the screen to name where the code goes, and
 * to warn when that is a program on this device, because any program on the
 * device could have started the request. A client that registered itself gets
 * a second warning: its name is its own word, where a metadata document's
 * host is something it cannot fake.
 *
 * Either answer leaves the application: the backend says where, which is the
 * program's own redirect address carrying a code or a refusal.
 */
export function ConsentRequestCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const [ deciding, setDeciding ] = useState<'approve' | 'deny' | null>(null)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const { request } = props

  async function decide(approve: boolean) {
    setDeciding(approve
      ? 'approve'
      : 'deny')
    setErrorMessage(null)
    try {
      const redirectTo = await api.decideAuthorizationRequest(request.id, approve)
      window.location.assign(redirectTo)
    }
    catch (error) {
      console.debug('the consent decision failed', error)
      // A request that expired while the page sat open answers 404, whose
      // server message is a bare "Not found."
      const refusalKey = consentRefusalKey(error)
      setErrorMessage(refusalKey
        ? t(refusalKey)
        : getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
      setDeciding(null)
    }
  }

  return <div>
    <div className='relaxed'>
      { request.client.verifiedHost
        ? <p className='opacity-80'>{
            t('oauth.consent.verifiedBy', { host: request.client.verifiedHost })
          }</p>
        : <Alert
            color='warning'
            title={t('oauth.consent.unverified')}
          />
      }
    </div>
    <div className='relaxed'>
      <p className='compact'>{
          t('oauth.consent.redirect', { host: request.redirectHost })
        }</p>
      { request.redirectIsLoopback
        ? <Alert
            color='warning'
            title={t('oauth.consent.loopbackWarning')}
          />
        : null
      }
    </div>
    <div className='relaxed'>
      { request.scopes.length
        ? <>
            <p className='compact font-semibold'>{
                t('oauth.consent.permissions')
              }</p>
            <ul className='list-disc pl-6'>{
                request.scopes.map((scope) => (
                  <li key={scope.name}>
                    <span>{scope.description}</span>
                    <code className='ml-2 text-xs opacity-60'>{scope.name}</code>
                  </li>
                ))
              }</ul>
          </>
        : <p>{
            t('oauth.consent.identityOnly')
          }</p>
      }
    </div>
    { user
      ? <p className='relaxed text-sm opacity-70'>{
          t('oauth.consent.signedInAs', { email: user.email })
        }</p>
      : null
    }
    { errorMessage
      ? <p className='relaxed text-danger'>{
          errorMessage
        }</p>
      : null
    }
    <div className='level-right gap-2'>
      <Button
        variant='light'
        isDisabled={Boolean(deciding)}
        isLoading={deciding === 'deny'}
        onPress={() => decide(false)}
      >
        <span>{
            t('oauth.consent.deny')
          }</span>
      </Button>
      <Button
        color='primary'
        isDisabled={Boolean(deciding)}
        isLoading={deciding === 'approve'}
        onPress={() => decide(true)}
      >
        <span>{
            t('oauth.consent.approve')
          }</span>
      </Button>
    </div>
  </div>
}
