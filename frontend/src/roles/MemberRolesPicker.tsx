// Copyright © 2026 Jalapeno Labs

import type { GrantTier, Role, TenantAccess } from '../api/types'
import type { AssignabilityRefusal } from './permissions'

// User interface
import { Checkbox, CheckboxGroup } from '@heroui/react'

// Misc
import { assignability, outranks } from './permissions'

/**
 * The picker's own strings.
 *
 * The package never imports i18next, so the defaults are English and a
 * translated page passes its own.
 */
export type MemberRolesPickerLabels = {
  /** The group's label and accessible name. */
  roles: string
  /** Why a role cannot be ticked or unticked by this viewer. */
  refusals: Record<Exclude<AssignabilityRefusal, 'default'>, string>
}

type Props = {
  /** The organization's roles, as the catalog answers them. */
  roles: Role[]
  /** The viewer's standing where the roles are granted. */
  viewer: TenantAccess
  /** Where the roles are granted. */
  tier: GrantTier
  /** The ids ticked now. */
  value: string[]
  onChange: (roleIds: string[]) => void
  isDisabled?: boolean
  labels?: Partial<MemberRolesPickerLabels>
}

const DEFAULT_LABELS: MemberRolesPickerLabels = {
  roles: 'Roles',
  refusals: {
    'wrong-tier': 'Granted at another level',
    'outranked': 'Ranks at or above your highest role',
    'lacks-permissions': 'Carries permissions you do not hold',
  },
}

/**
 * The roles a member holds somewhere, ticked on and off.
 *
 * The default role is everybody's and never offered. Every other role is
 * listed, highest first, and one the viewer may not hand out is shown locked
 * with the reason beneath it, never hidden, so a manager can see what holds
 * them back. A role a member already holds that the viewer outranks may be
 * taken away even when it carries something the viewer lacks, because taking
 * a role away gives nobody anything.
 */
export function MemberRolesPicker(props: Props) {
  const labels = {
    ...DEFAULT_LABELS,
    ...props.labels,
    refusals: { ...DEFAULT_LABELS.refusals, ...props.labels?.refusals },
  }
  const held = new Set(props.value)
  const offered = props.roles.filter((role) => !role.isDefault)

  return <CheckboxGroup
    label={labels.roles}
    value={props.value}
    onValueChange={props.onChange}
    isDisabled={props.isDisabled}
  >
    {
      offered.map((role) => {
        const refusal = assignability(props.viewer, role, props.tier)
        // Unticking a held role is a revocation, which the hierarchy alone
        // decides; ticking one is a grant, which must carry nothing new.
        const mayUntick = held.has(role.id) && outranks(props.viewer, role)
        const isLocked = refusal !== null && !mayUntick

        return <Checkbox
          key={role.id}
          value={role.id}
          isDisabled={isLocked}
          classNames={{ label: 'w-full' }}
        >
          <span className='flex items-center gap-2'>
            <span
              aria-hidden='true'
              className='h-3 w-3 shrink-0 rounded-full'
              style={{ backgroundColor: role.color }}
            />
            <span>{
                role.name
              }</span>
          </span>
          { isLocked && refusal !== 'default' && refusal !== null
            ? <span className='block text-small opacity-60'>{
                labels.refusals[refusal]
              }</span>
            : null
          }
        </Checkbox>
      })
    }
  </CheckboxGroup>
}
