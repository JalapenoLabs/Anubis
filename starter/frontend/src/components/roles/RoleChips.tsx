// Copyright © 2026 Jalapeno Labs

import type { RoleSummary } from '@jalapenolabs/anubis'

// Core
import { useTranslation } from 'react-i18next'

// UI
import { Chip } from '@heroui/react'

type Props = {
  roles: RoleSummary[]
}

/**
 * The roles somebody holds, each in its own colour, highest first.
 *
 * Holding none of them still means holding the default role, so an empty
 * list reads as that rather than as nothing.
 */
export function RoleChips(props: Props) {
  const { t } = useTranslation()

  if (!props.roles.length) {
    return <span className='opacity-60'>{
        t('roles.onlyDefault')
      }</span>
  }

  return <div className='flex flex-wrap gap-1'>
    {
      props.roles.map((role) => (
        <Chip
          key={role.id}
          size='sm'
          variant='flat'
          startContent={
            <span
              aria-hidden='true'
              className='ml-1 h-2 w-2 shrink-0 rounded-full'
              style={{ backgroundColor: role.color }}
            />
          }
        >{
            role.name
          }</Chip>
      ))
    }
  </div>
}
