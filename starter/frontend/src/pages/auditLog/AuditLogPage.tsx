// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'
import { findTeam, useTeamContext } from '../../context/TeamProvider'

// UI
import { Spinner } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { AuditEventsCard } from '../../components/auditLog/AuditEventsCard'

// Misc
import { ADMIN_ROLE } from '../../permissions'
import { getOrganizationSettingsUrl, getTeamSettingsUrl } from '../../urls'

/**
 * One team's audit log: every act the framework and the app recorded for it.
 *
 * Team-scoped and admin-gated, because that is what the endpoint enforces: the
 * log shows every member's activity, which is an administrative view of the
 * team rather than an editorial one.
 */
export function AuditLogPage() {
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
      { label: t('auditLog.title') },
    ]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('auditLog.title')
        }</h2>
      <p className='opacity-70'>{
          t('auditLog.pageSubtitle', { team: team.name })
        }</p>
    </div>
    <AuditEventsCard
      teamId={team.id}
      canRead={team.roles.includes(ADMIN_ROLE)}
    />
  </AppShell>
}
