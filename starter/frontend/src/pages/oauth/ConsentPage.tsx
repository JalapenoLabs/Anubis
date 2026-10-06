// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { useAnubisApi } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { AuthLayout } from '../../components/AuthLayout'
import { ConsentRequestCard } from '../../components/oauth/ConsentRequestCard'

// Utility
import { HTTPError } from 'ky'

type Props = {
  requestId: string
}

/**
 * Asks the signed-in person whether a program such as Claude Code may act as them.
 *
 * The backend validated the program's request and stored it before sending the
 * browser here, so the page reads it by id and never trusts a parameter the
 * program could have edited. A request answers `404` once it expired or was
 * decided, which the page says in those words rather than as a failure.
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

  const clientName = request.data?.client.name ?? ''
  const isGone = !props.requestId
    || (request.error instanceof HTTPError && request.error.response.status === 404)

  return <AuthLayout
    title={t('oauth.consent.title', { client: clientName })}
    subtitle={request.data
      ? t('oauth.consent.subtitle', { client: clientName })
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
    { isGone
      ? <p className='text-danger'>{
          t('oauth.consent.expired')
        }</p>
      : null
    }
    { request.error && !isGone
      ? <p className='text-danger'>{
          t('common.somethingWentWrong')
        }</p>
      : null
    }
    { request.data
      ? <ConsentRequestCard request={request.data} />
      : null
    }
  </AuthLayout>
}
