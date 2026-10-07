// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { getApiErrorMessage, useAnubisApi } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { Link } from 'react-router'
import { AuthLayout } from '../../components/AuthLayout'
import { ConsentRequestCard } from '../../components/oauth/ConsentRequestCard'

// Misc
import { UrlTree } from '../../urls'
import { consentRefusalKey } from '../../components/oauth/consentRefusal'

type Props = {
  requestId: string
}

/**
 * Asks the signed-in person whether a program such as Claude Code may act as them.
 *
 * The backend validated the program's request and stored it before sending the
 * browser here, so the page reads it by id and never trusts a parameter the
 * program could have edited. A request answers `404` once it expired or was
 * decided, which the page says in those words rather than as a failure, and
 * an account on a temporary password is sent to choose its own first.
 */
export function ConsentPage(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const request = useSWR(
    props.requestId
      ? [ 'oauth/requests', props.requestId ]
      : null,
    () => api.getAuthorizationRequest(props.requestId),
    { shouldRetryOnError: false },
  )

  const refusalKey = props.requestId
    ? consentRefusalKey(request.error)
    : 'oauth.consent.expired'

  return <AuthLayout
    title={request.data
      ? t('oauth.consent.title', { client: request.data.client.name })
      : t('oauth.consent.titleUnknown')}
    subtitle={request.data
      ? t('oauth.consent.subtitle', { client: request.data.client.name })
      : ''}
  >
    { request.isLoading
      ? <div className='level'>
          <Spinner size='sm' />
          <p className='w-full'>{
              t('common.loading')
            }</p>
        </div>
      : null
    }
    { refusalKey
      ? <p className='compact text-danger'>{
          t(refusalKey)
        }</p>
      : null
    }
    { refusalKey === 'oauth.consent.passwordChangeRequired'
      ? <Link to={UrlTree.settingsSecurity} className='text-primary'>{
          t('oauth.consent.choosePassword')
        }</Link>
      : null
    }
    { request.error && !refusalKey
      ? <p className='text-danger'>{
          getApiErrorMessage(request.error) ?? t('common.somethingWentWrong')
        }</p>
      : null
    }
    { request.data
      ? <ConsentRequestCard request={request.data} />
      : null
    }
  </AuthLayout>
}
