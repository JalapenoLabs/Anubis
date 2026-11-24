// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import useSWR from 'swr'
import { MEMBERSHIP_SUBJECT, allows, useAnubisApi, useCurrentUser, useRoles } from '@jalapenolabs/anubis'
import { findTeam, useTeamContext } from '../../context/TeamProvider'

// UI
import { Spinner } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { InviteMemberCard } from '../../components/tenancy/InviteMemberCard'
import { RenameTenantCard } from '../../components/tenancy/RenameTenantCard'
import { TeamDangerZoneCard } from '../../components/tenancy/TeamDangerZoneCard'
import { TeamRosterCard } from '../../components/tenancy/TeamRosterCard'

// Misc
import { getOrganizationSettingsUrl } from '../../urls'

/**
 * Administering one team: its name, its roster, and the way out of it.
 *
 * The organization the team belongs to comes from the membership overview
 * rather than from a second request, which is also what tells the page whether
 * the viewer may dissolve the team: that is an organization administrator's
 * call. The roles offered on the roster and in an invitation are read through
 * the team, with the viewer's standing there.
 */
export function TeamSettingsPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { teamId = '' } = useParams()

  const roster = useSWR(
    teamId ? `team-members/${teamId}` : null,
    () => api.listTeamMembers(teamId),
  )
  const roles = useRoles(teamId ? { teamId } : null)

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
  const isTeamAdmin = team.access.isAdministrator
  const isOrganizationAdmin = organization.access.isAdministrator
  // The organization's managers have always been able to staff its teams.
  const mayInvite = allows(team.access, 'create', MEMBERSHIP_SUBJECT)
    || allows(organization.access, 'create', MEMBERSHIP_SUBJECT)

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: team.name },
    ]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('team.settings.title')
        }</h2>
      <p className='opacity-70'>{
          t('team.settings.subtitle', { team: team.name })
        }</p>
    </div>
    <RenameTenantCard
      title={t('team.settings.nameTitle')}
      subtitle={t('team.settings.nameSubtitle')}
      label={t('team.settings.nameLabel')}
      name={team.name}
      canRename={isTeamAdmin}
      onRename={async (name) => {
        await api.renameTeam(team.id, name)
        await memberships.refresh()
      }}
    />
    <TeamRosterCard
      teamId={team.id}
      members={roster.data ?? []}
      isLoading={roster.isLoading}
      catalog={roles.catalog}
      currentUserEmail={user.email}
      onChanged={() => {
        void roster.mutate()
        void memberships.refresh()
      }}
    />
    { mayInvite
      ? <InviteMemberCard
          title={t('team.members.inviteTitle')}
          teamId={team.id}
          catalog={roles.catalog}
          organizationViewer={organization.access}
          onInvited={() => {
            void roster.mutate()
          }}
        />
      : null
    }
    <TeamDangerZoneCard
      teamId={team.id}
      teamName={team.name}
      organizationId={organization.id}
      canDeleteTeam={isOrganizationAdmin}
      onChanged={memberships.refresh}
    />
  </AppShell>
}
