// Copyright © 2026 Jalapeno Labs

import type { RoleFormValues } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { getApiErrorMessage, useAnubisApi, useCurrentUser, useRoles } from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'
import { useRoleLabels } from '../../components/roles/useRoleLabels'

// UI
import { Button, Card, CardBody, Spinner } from '@heroui/react'
import { Link } from 'react-router'
import { RoleForm } from '@jalapenolabs/anubis'
import { AppShell } from '../../components/AppShell'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'

// Misc
import { roleSchema } from '../../components/roles/roleSchema'
import {
  getOrganizationRoleUrl,
  getOrganizationRolesUrl,
  getOrganizationSettingsUrl,
} from '../../urls'

const resolver = zodResolver(roleSchema)

/** Where an unsaved draft waits across a reload, per organization. */
function draftKey(organizationId: string) {
  return `anubis.roleDraft.${organizationId}`
}

/** A draft from before a reload, or nothing; storage may refuse to answer. */
function readDraft(organizationId: string): RoleFormValues | null {
  try {
    const stored = window.sessionStorage.getItem(draftKey(organizationId))
    if (!stored) {
      return null
    }
    const parsed = roleSchema.safeParse(JSON.parse(stored))
    return parsed.success
      ? parsed.data
      : null
  }
  catch (error) {
    console.debug('NewRolePage could not read its draft', error)
    return null
  }
}

/**
 * Making a role, on a page of its own.
 *
 * A new role lands directly above the default role, where it outranks nobody,
 * and starts with the permissions chosen here, every one of which the viewer
 * must hold. The draft survives a reload, and is forgotten once saved.
 */
export function NewRolePage() {
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

  const form = useForm<RoleFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: readDraft(organizationId) ?? {
      name: '',
      color: '#7c3aed',
      permissions: [],
    },
  })

  const values = form.watch()
  useEffect(() => {
    if (!values.name.trim()) {
      return
    }
    try {
      window.sessionStorage.setItem(draftKey(organizationId), JSON.stringify(values))
    }
    catch (error) {
      console.debug('NewRolePage could not keep its draft', error)
    }
  }, [ organizationId, values ])

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

  const onSubmit = form.handleSubmit(async (draft) => {
    setErrorMessage(null)
    try {
      const created = await api.createRole(organizationId, draft)
      try {
        window.sessionStorage.removeItem(draftKey(organizationId))
      }
      catch (error) {
        console.debug('NewRolePage could not forget its draft', error)
      }
      await roles.refresh()
      navigate(getOrganizationRoleUrl(organizationId, created.id))
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
  })

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('roles.title'), to: getOrganizationRolesUrl(organization.id) },
      { label: t('roles.newTitle') },
    ]}
  >
    <h2 className='relaxed title'>{
        t('roles.newTitle')
      }</h2>
    <form onSubmit={onSubmit}>
      <Card className='relaxed p-2'>
        <CardBody>
          { roles.catalog
            ? <RoleForm
                control={form.control}
                vocabulary={roles.catalog.permissions}
                grantable={roles.catalog.grantable}
                viewer={roles.catalog.viewer}
                isDisabled={form.formState.isSubmitting}
                autoFocus
                subjectLabel={labels.subjectLabel}
                labels={labels.form}
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
      <div className='level'>
        <Button
          as={Link}
          to={getOrganizationRolesUrl(organization.id)}
          variant='bordered'
        >
          <span>{
              t('roles.back')
            }</span>
        </Button>
        <Button
          type='submit'
          color='primary'
          isDisabled={!form.formState.isValid || form.formState.isSubmitting || !roles.catalog}
          isLoading={form.formState.isSubmitting}
        >
          <span>{
              t('roles.createAction')
            }</span>
        </Button>
      </div>
    </form>
  </AppShell>
}
