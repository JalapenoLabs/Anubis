// Copyright © 2026 Jalapeno Labs

import type { AnubisApi, ClaimedInvitation } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { useAnubisApi, useMemberships } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { AuthLayout } from '../components/AuthLayout'

// Misc
import { UrlTree } from '../urls'

// Invitation tokens are single-use, so claim each token exactly once per
// page load; StrictMode's development double-mount must not consume the
// token twice.
const claimsByToken = new Map<string, Promise<ClaimedInvitation>>()

function claimTokenOnce(api: AnubisApi, token: string): Promise<ClaimedInvitation> {
  let claim = claimsByToken.get(token)
  if (!claim) {
    claim = api.claimInvitation(token)
    claimsByToken.set(token, claim)
  }
  return claim
}

type ClaimState =
  | { phase: 'claiming' }
  | { phase: 'joined', claimed: ClaimedInvitation }
  | { phase: 'failed' }

export function ClaimInvitationPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useMemberships()
  const [ searchParams ] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  const [ state, setState ] = useState<ClaimState>({ phase: 'claiming' })

  useEffect(() => {
    let cancelled = false

    if (!token) {
      setState({ phase: 'failed' })
    }
    else {
      claimTokenOnce(api, token)
        .then(async (claimed) => {
          await refresh()
          if (!cancelled) {
            setState({
              phase: 'joined',
              claimed,
            })
          }
        })
        .catch((error: unknown) => {
          console.debug('claiming the invitation failed', error)
          if (!cancelled) {
            setState({ phase: 'failed' })
          }
        })
    }

    return () => {
      cancelled = true
    }
    // The token is fixed for the lifetime of the page.
  }, [])

  return <AuthLayout
    title={t('team.claim.title')}
    subtitle=''
  >
    { state.phase === 'claiming'
      ? <div className='level'>
          <Spinner size='sm' />
          <p className='w-full'>{
              t('team.claim.claiming')
            }</p>
        </div>
      : null
    }
    { state.phase === 'joined'
      ? <p className='relaxed'>{
          state.claimed.team
            ? t('team.claim.joinedTeam', {
              team: state.claimed.team.name,
              organization: state.claimed.organization.name,
            })
            : t('team.claim.joinedOrganization', {
              organization: state.claimed.organization.name,
            })
        }</p>
      : null
    }
    { state.phase === 'failed'
      ? <p className='relaxed text-danger'>{
          t('team.claim.failed')
        }</p>
      : null
    }
    <div className='level-right mt-4 text-sm'>
      <Link to={UrlTree.root} className='text-primary'>{
          t('team.claim.goToDashboard')
        }</Link>
    </div>
  </AuthLayout>
}
