// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'
import type { PermissionAction } from './permissions'
import type { Control } from 'react-hook-form'

// Core
import { useController } from 'react-hook-form'

// User interface
import { Switch, Tooltip } from '@heroui/react'
import { ColorPickerField } from '../fields/ColorPickerField'
import { TextField } from '../fields/TextField'

// Misc
import { PERMISSION_ACTIONS, groupPermissions, holdsAll } from './permissions'

/** What the role form edits. */
export type RoleFormValues = {
  name: string
  /** `#rrggbb`. */
  color: string
  /** Permission keys, `<Subject>.<action>`. */
  permissions: string[]
}

/**
 * The role form's own strings.
 *
 * The package never imports i18next, so the defaults are English and a
 * translated page passes its own.
 */
export type RoleFormLabels = {
  name: string
  color: string
  /** The permission table's heading and accessible name. */
  permissions: string
  /** The table's first column heading. */
  subject: string
  /** Said instead of the toggles on the administrator role. */
  administratorNote: string
  /** Said above the toggles on the default role. */
  defaultNote: string
  /** Why a toggle is locked: the viewer does not hold that permission. */
  notHeld: string
  /** Why a toggle is locked: no role in the application grants that permission. */
  notGrantable: string
  /** The action column headings. */
  actions: Record<PermissionAction, string>
}

type Props = {
  /** The page's form; the fields bind to `name`, `color`, and `permissions`. */
  control: Control<RoleFormValues>
  /** Every permission key a role may carry, as the catalog answers them. */
  vocabulary: readonly string[]
  /** The keys a role can be given at all, as the catalog answers them. */
  grantable: readonly string[]
  /** The viewer's organization-wide standing: they switch only what they hold. */
  viewer: TenantAccess
  /** The role being edited, or nothing for a new one. */
  role?: Role
  isDisabled?: boolean
  /** Puts the cursor in the name field when the form appears. */
  autoFocus?: boolean
  /**
   * How a subject reads. The framework's own subjects and the application's
   * models are identifiers; the page turns them into words.
   */
  subjectLabel?: (subject: string) => string
  labels?: Partial<RoleFormLabels>
}

const DEFAULT_LABELS: RoleFormLabels = {
  name: 'Name',
  color: 'Color',
  permissions: 'Permissions',
  subject: 'Area',
  administratorNote: 'The administrator role holds every permission.',
  defaultNote: 'Everybody holds this role, so every permission here is everybody\'s.',
  notHeld: 'You can only switch permissions you hold.',
  notGrantable: 'No role in this application grants this permission.',
  actions: {
    read: 'Read',
    create: 'Create',
    update: 'Update',
    destroy: 'Delete',
  },
}

/**
 * A role's name, colour, and permissions, as switches by area and action.
 *
 * A switch is locked unless the viewer holds that permission, which is the
 * server's rule for every edit: nobody switches a permission on or off that
 * they do not hold. One no role in the application grants is locked off for
 * everybody, the administrator included, because the server refuses to give
 * it; one a role already carries can still be switched off. The administrator
 * role's permissions are all of them and are not edited, so its table is
 * shown and locked.
 */
export function RoleForm(props: Props) {
  const labels = { ...DEFAULT_LABELS, ...props.labels }
  const subjectLabel = props.subjectLabel ?? ((subject: string) => subject)
  const { field } = useController({ control: props.control, name: 'permissions' })
  const held = new Set(field.value)
  const grantable = new Set(props.grantable)
  const groups = groupPermissions(props.vocabulary)
  const locked = Boolean(props.isDisabled || props.role?.isAdministrator)

  function toggle(key: string, isOn: boolean) {
    const next = new Set(held)
    if (isOn) {
      next.add(key)
    }
    else {
      next.delete(key)
    }
    // Kept in the vocabulary's order, so a saved role reads the same way twice.
    field.onChange(props.vocabulary.filter((candidate) => next.has(candidate)))
  }

  return <div>
    <div className='compact grid gap-3 sm:grid-cols-2'>
      <TextField
        control={props.control}
        name='name'
        id='role-name'
        label={labels.name}
        isRequired
        isDisabled={props.isDisabled}
        autoFocus={props.autoFocus}
      />
      <ColorPickerField
        control={props.control}
        name='color'
        id='role-color'
        label={labels.color}
        isRequired
        isDisabled={props.isDisabled}
      />
    </div>
    <h4 className='compact mt-4 text-lg font-semibold'>{
        labels.permissions
      }</h4>
    { props.role?.isAdministrator
      ? <p className='compact opacity-70'>{
          labels.administratorNote
        }</p>
      : null
    }
    { props.role?.isDefault
      ? <p className='compact opacity-70'>{
          labels.defaultNote
        }</p>
      : null
    }
    <div className='overflow-x-auto'>
      <table
        aria-label={labels.permissions}
        className='w-full text-left'
      >
        <thead>
          <tr>
            <th className='py-2 pr-4 font-medium opacity-70'>{
                labels.subject
              }</th>
            {
              PERMISSION_ACTIONS.map((action) => (
                <th key={action} className='px-2 py-2 text-center font-medium opacity-70'>{
                    labels.actions[action]
                  }</th>
              ))
            }
          </tr>
        </thead>
        <tbody>
          {
            groups.map((group) => (
              <tr key={group.subject} className='border-t border-default-200'>
                <th scope='row' className='py-2 pr-4 font-normal'>{
                    subjectLabel(group.subject)
                  }</th>
                {
                  PERMISSION_ACTIONS.map((action) => {
                    const key = group.keys[action]
                    if (!key) {
                      return <td key={action} className='px-2 text-center opacity-30'>-</td>
                    }

                    const ungrantable = !grantable.has(key) && !held.has(key)
                    const lackingIt = !holdsAll(props.viewer, [ key ])
                    const reason = ungrantable
                      ? labels.notGrantable
                      : labels.notHeld
                    const toggleSwitch = <Switch
                      size='sm'
                      aria-label={`${subjectLabel(group.subject)}: ${labels.actions[action]}`}
                      isSelected={held.has(key)}
                      isDisabled={locked || lackingIt || ungrantable}
                      onValueChange={(isOn) => toggle(key, isOn)}
                    />

                    return <td key={action} className='px-2 text-center'>{
                        (lackingIt || ungrantable) && !locked
                          ? <Tooltip content={reason}>
                              <div className='inline-block'>{
                                  toggleSwitch
                                }</div>
                            </Tooltip>
                          : toggleSwitch
                      }</td>
                  })
                }
              </tr>
            ))
          }
        </tbody>
      </table>
    </div>
  </div>
}
