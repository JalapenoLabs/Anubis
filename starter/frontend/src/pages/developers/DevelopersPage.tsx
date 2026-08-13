// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'
import { findTeam, useTeamContext } from '../../context/TeamProvider'

// UI
import { Spinner } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { WebhookEndpointsCard } from '../../components/developers/WebhookEndpointsCard'

// Misc
import { ADMIN_ROLE } from '../../permissions'
import { getOrganizationSettingsUrl, getTeamSettingsUrl } from '../../urls'

/**
 * The Developers section for one team: where its integrations are wired up.
 *
 * Outgoing webhooks live here today. The surface is team-scoped and
 * admin-gated because that is what the API enforces: a subscription decides
 * where a team's records are sent.
 *
 * The client route is `/teams/:teamId/developers` rather than `/developers`,
 * which the backend reserves for the API itself; a client route under a
 * reserved prefix would answer JSON on a cold load.
 */
export function DevelopersPage() {
  const { t } = useTranslation()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { teamId = '' } = useParams()

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  const found = findTeam(memberships.organizations, teamId)

  if (!found) {
    return <AppShell user={user}>
      { memberships.isLoading
        ? <Spinner />
        : <p className='opacity-70'>{
            t('team.settings.notFound')
          }</p>
      }
    </AppShell>
  }

  const { organization, team } = found

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: team.name, to: getTeamSettingsUrl(team.id) },
      { label: t('developers.title') },
    ]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('developers.title')
        }</h2>
      <p className='opacity-70'>{
          t('developers.subtitle', { team: team.name })
        }</p>
    </div>
    <WebhookEndpointsCard
      teamId={team.id}
      canManage={team.roles.includes(ADMIN_ROLE)}
    />
  </AppShell>
}
