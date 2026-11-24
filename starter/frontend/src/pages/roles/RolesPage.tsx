// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import {
  ROLE_SUBJECT,
  allows,
  getApiErrorMessage,
  useAnubisApi,
  useCurrentUser,
  useRoles,
} from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'
import { useRoleLabels } from '../../components/roles/useRoleLabels'

// UI
import { Button, Card, CardBody, Spinner } from '@heroui/react'
import { Link } from 'react-router'
import { RoleLadder } from '@jalapenolabs/anubis'
import { AppShell } from '../../components/AppShell'

// Misc
import {
  getOrganizationRoleNewUrl,
  getOrganizationRoleUrl,
  getOrganizationSettingsUrl,
} from '../../urls'

/**
 * An organization's roles, highest first, and the way to make another.
 *
 * Every member reads the ladder, because rosters name these roles; the
 * ladder offers to move only what the viewer may move, and a new role only to
 * somebody allowed to make one.
 */
export function RolesPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const labels = useRoleLabels()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '' } = useParams()
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const organization = memberships.organizations.find(
    (candidate) => candidate.id === organizationId,
  )
  const roles = useRoles(organization?.isMember ? { organizationId } : null)

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (!organization?.isMember) {
    return <AppShell user={user}>
      { memberships.isLoading
        ? <Spinner />
        : <p className='opacity-70'>{
            t('organization.settings.notFound')
          }</p>
      }
    </AppShell>
  }

  const catalog = roles.catalog
  const mayCreate = Boolean(catalog && allows(catalog.viewer, 'create', ROLE_SUBJECT))
  const mayReorder = Boolean(catalog && allows(catalog.viewer, 'update', ROLE_SUBJECT))

  // The ladder shows the new order the moment it is dropped; a refusal puts
  // it back and says why, in place, until the next move.
  async function onReorder(roleIds: string[]) {
    setErrorMessage(null)
    try {
      await api.reorderRoles(organizationId, roleIds)
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      await roles.refresh()
    }
  }

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('roles.title') },
    ]}
  >
    <div className='relaxed level'>
      <h2 className='title'>{
          t('roles.title')
        }</h2>
      { mayCreate
        ? <Button
            as={Link}
            to={getOrganizationRoleNewUrl(organization.id)}
            color='primary'
          >
            <span>{
                t('roles.newAction')
              }</span>
          </Button>
        : null
      }
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        { catalog
          ? <RoleLadder
              roles={catalog.roles}
              viewer={mayReorder
                ? catalog.viewer
                : { ...catalog.viewer, rank: 0, isAdministrator: false }}
              hrefFor={(role) => getOrganizationRoleUrl(organization.id, role.id)}
              onNavigate={(href) => navigate(href)}
              onReorder={onReorder}
              labels={labels.ladder}
            />
          : <Spinner />
        }
        { errorMessage
          ? <p className='mt-4 text-danger'>{
              errorMessage
            }</p>
          : null
        }
      </CardBody>
    </Card>
  </AppShell>
}
