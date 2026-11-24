// Copyright © 2026 Jalapeno Labs

import type { Role, RoleCatalog, RoleFormValues, RoleUpdate } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Navigate, useNavigate, useParams } from 'react-router'
import {
  ROLE_SUBJECT,
  allows,
  getApiErrorMessage,
  outranks,
  useAnubisApi,
  useCurrentUser,
  useRoles,
} from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'
import { useRoleLabels } from '../../components/roles/useRoleLabels'

// UI
import { Button, Card, CardBody, Spinner, Tooltip } from '@heroui/react'
import { RoleForm } from '@jalapenolabs/anubis'
import { AppShell } from '../../components/AppShell'
import { ConfirmActionModal } from '../../components/tenancy/ConfirmActionModal'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'

// Misc
import { roleSchema } from '../../components/roles/roleSchema'
import { getOrganizationRolesUrl, getOrganizationSettingsUrl } from '../../urls'

const resolver = zodResolver(roleSchema)

/**
 * One role's own page: its name, colour, and permissions, saved in the top
 * right, and the way to delete it.
 *
 * Fields the viewer may not change are shown, locked, rather than hidden: a
 * role above the viewer is read-only, a permission they do not hold is a
 * locked switch, and the administrator role's permissions are all of them.
 */
export function RolePage() {
  const { t } = useTranslation()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '', roleId = '' } = useParams()

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

  if (!roles.catalog) {
    return <AppShell user={user}>
      <Spinner />
    </AppShell>
  }

  const role = roles.catalog.roles.find((candidate) => candidate.id === roleId)
  if (!role) {
    // A role deleted meanwhile, or never this organization's: one level up.
    return <Navigate to={getOrganizationRolesUrl(organizationId)} replace />
  }

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('roles.title'), to: getOrganizationRolesUrl(organization.id) },
      { label: role.name },
    ]}
  >
    <EditRole
      key={role.id}
      organizationId={organization.id}
      role={role}
      catalog={roles.catalog}
      onChanged={async () => {
        await roles.refresh()
        await memberships.refresh()
      }}
    />
  </AppShell>
}

type EditRoleProps = {
  organizationId: string
  role: Role
  catalog: RoleCatalog
  onChanged: () => Promise<unknown>
}

/** The form itself, mounted once the role is known so it starts from it. */
function EditRole(props: EditRoleProps) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const labels = useRoleLabels()
  const [ isSaved, setIsSaved ] = useState(false)
  const [ isConfirmingDelete, setIsConfirmingDelete ] = useState(false)

  const { role, catalog } = props
  const viewer = catalog.viewer
  const mayEdit = allows(viewer, 'update', ROLE_SUBJECT) && outranks(viewer, role)
  const mayDelete = allows(viewer, 'destroy', ROLE_SUBJECT)
    && outranks(viewer, role)
    && !role.isDefault
    && !role.isAdministrator

  const form = useForm<RoleFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      name: role.name,
      color: role.color,
      permissions: role.permissions,
    },
  })

  // A save, or somebody else's edit, changes the role underneath the form;
  // the form follows what the server has.
  const { reset } = form
  useEffect(() => {
    reset({ name: role.name, color: role.color, permissions: role.permissions })
  }, [ role, reset ])

  const onSave = form.handleSubmit(async (values) => {
    form.clearErrors('root')
    setIsSaved(false)

    const update: RoleUpdate = {}
    if (values.name.trim() !== role.name) {
      update.name = values.name.trim()
    }
    if (values.color.toLowerCase() !== role.color) {
      update.color = values.color.toLowerCase()
    }
    const permissionsChanged = values.permissions.length !== role.permissions.length
      || values.permissions.some((key) => !role.permissions.includes(key))
    if (permissionsChanged && !role.isAdministrator) {
      update.permissions = values.permissions
    }

    try {
      await api.updateRole(props.organizationId, role.id, update)
      await props.onChanged()
      setIsSaved(true)
    }
    catch (error) {
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
  })

  async function onFollowRecommended() {
    form.clearErrors('root')
    setIsSaved(false)
    try {
      await api.updateRole(props.organizationId, role.id, { permissions: null })
      await props.onChanged()
      setIsSaved(true)
    }
    catch (error) {
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
  }

  const isDirty = form.formState.isDirty
  const saveDisabledReason = !isDirty
    ? t('roles.nothingChanged')
    : null

  return <form onSubmit={onSave}>
    <div className='relaxed level'>
      <h2 className='title flex items-center gap-3'>
        <span
          aria-hidden='true'
          className='h-4 w-4 rounded-full'
          style={{ backgroundColor: role.color }}
        />
        <span>{
            role.name
          }</span>
      </h2>
      { mayEdit
        ? <Tooltip content={saveDisabledReason} isDisabled={!saveDisabledReason}>
            <div>
              <Button
                type='submit'
                color='primary'
                isDisabled={!isDirty || !form.formState.isValid || form.formState.isSubmitting}
                isLoading={form.formState.isSubmitting}
              >
                <span>{
                    t('common.save')
                  }</span>
              </Button>
            </div>
          </Tooltip>
        : null
      }
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        <RoleForm
          control={form.control}
          vocabulary={catalog.permissions}
          viewer={viewer}
          role={role}
          isDisabled={!mayEdit || form.formState.isSubmitting}
          subjectLabel={labels.subjectLabel}
          labels={labels.form}
        />
        { form.formState.errors.root
          ? <p className='mt-4 text-danger'>{
              form.formState.errors.root.message
            }</p>
          : null
        }
        { isSaved
          ? <p className='mt-4 opacity-80'>{
              t('roles.saved')
            }</p>
          : null
        }
        { mayEdit && role.key && role.customized && !role.isAdministrator
          ? <div className='mt-4'>
              <Button
                variant='flat'
                onPress={onFollowRecommended}
              >
                <span>{
                    t('roles.followRecommended')
                  }</span>
              </Button>
            </div>
          : null
        }
      </CardBody>
    </Card>
    { mayDelete
      ? <div className='container max-w-md rounded-medium border border-danger p-4'>
          <h3 className='compact font-semibold text-danger'>{
              t('roles.dangerTitle')
            }</h3>
          <Button
            color='danger'
            variant='flat'
            className='w-full'
            onPress={() => setIsConfirmingDelete(true)}
          >
            <span>{
                t('roles.deleteAction')
              }</span>
          </Button>
        </div>
      : null
    }
    <ConfirmActionModal
      isOpen={isConfirmingDelete}
      title={t('roles.deleteModalTitle', { role: role.name })}
      body={t('roles.deleteModalBody', { role: role.name })}
      confirmLabel={t('roles.deleteAction')}
      onConfirm={async () => {
        await api.deleteRole(props.organizationId, role.id)
        await props.onChanged()
        navigate(getOrganizationRolesUrl(props.organizationId))
      }}
      onClose={() => setIsConfirmingDelete(false)}
    />
  </form>
}
