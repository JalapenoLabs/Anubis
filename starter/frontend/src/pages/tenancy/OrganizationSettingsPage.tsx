// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import useSWR from 'swr'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'

// UI
import { Button, Card, CardBody, Spinner } from '@heroui/react'
import { Link } from 'react-router'
import { AppShell } from '../../components/AppShell'
import { InviteMemberCard } from '../../components/tenancy/InviteMemberCard'
import { OrganizationDangerZoneCard } from '../../components/tenancy/OrganizationDangerZoneCard'
import { OrganizationRosterCard } from '../../components/tenancy/OrganizationRosterCard'
import { OrganizationTeamsCard } from '../../components/tenancy/OrganizationTeamsCard'
import { RenameTenantCard } from '../../components/tenancy/RenameTenantCard'

// Misc
import { ADMIN_ROLE } from '../../permissions'
import { getOrganizationBillingUrl } from '../../urls'

/**
 * Administering one organization: its name, its people, and its teams.
 *
 * The teams listed are the ones the membership overview knows, which are the
 * teams the viewer belongs to. Creating a team here puts the creator in it, so
 * an organization built through this screen stays fully visible from it.
 */
export function OrganizationSettingsPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '' } = useParams()

  const organization = memberships.organizations.find(
    (candidate) => candidate.id === organizationId,
  )

  // Somebody invited into a single team appears under its organization with no
  // organization roles at all, and the organization's own surface is closed to
  // them: asking for the roster would only earn a 404.
  const isOrganizationMember = Boolean(organization?.roles.length)

  const roster = useSWR(
    isOrganizationMember ? `organization-members/${organizationId}` : null,
    () => api.listOrganizationMembers(organizationId),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (!organization) {
    return <AppShell user={user}>
      { memberships.isLoading
        ? <Spinner />
        : <p className='opacity-70'>{
            t('organization.settings.notFound')
          }</p>
      }
    </AppShell>
  }

  const isAdmin = organization.roles.includes(ADMIN_ROLE)

  return <AppShell
    user={user}
    breadcrumbs={[{ label: organization.name }]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('organization.settings.title')
        }</h2>
      <p className='opacity-70'>{
          t('organization.settings.subtitle', { organization: organization.name })
        }</p>
    </div>
    <RenameTenantCard
      title={t('organization.settings.nameTitle')}
      subtitle={t('organization.settings.nameSubtitle')}
      label={t('organization.settings.nameLabel')}
      name={organization.name}
      canRename={isAdmin}
      onRename={async (name) => {
        await api.renameOrganization(organization.id, name)
        await memberships.refresh()
      }}
    />
    {/* Billing attaches to the organization, so its link belongs here rather
        than in a team's settings. Every member may open it: the plan explains
        what the whole organization can do. */}
    <Card className='relaxed p-2'>
      <CardBody>
        <div className='level items-start'>
          <div>
            <h3 className='compact text-xl font-semibold'>{
                t('billing.cardTitle')
              }</h3>
            <p className='opacity-70'>{
                t('billing.cardSubtitle')
              }</p>
          </div>
          <Button
            as={Link}
            to={getOrganizationBillingUrl(organization.id)}
            color='primary'
            variant='flat'
            className='shrink-0'
          >
            <span>{
                t('billing.cardAction')
              }</span>
          </Button>
        </div>
      </CardBody>
    </Card>
    <OrganizationTeamsCard
      organizationId={organization.id}
      teams={organization.teams}
      canManage={isAdmin}
      onChanged={memberships.refresh}
    />
    { isOrganizationMember
      ? <OrganizationRosterCard
          organizationId={organization.id}
          members={roster.data ?? []}
          isLoading={roster.isLoading}
          canManage={isAdmin}
          currentUserEmail={user.email}
          onChanged={() => {
            void roster.mutate()
          }}
        />
      : <p className='relaxed opacity-70'>{
          t('organization.settings.teamMemberOnly')
        }</p>
    }
    { isAdmin
      ? <InviteMemberCard
          title={t('organization.settings.inviteTitle')}
          organizationId={organization.id}
          onInvited={() => {
            void roster.mutate()
          }}
        />
      : null
    }
    { isOrganizationMember
      ? <OrganizationDangerZoneCard
          organizationId={organization.id}
          organizationName={organization.name}
          canDelete={isAdmin}
          onChanged={memberships.refresh}
        />
      : null
    }
  </AppShell>
}
