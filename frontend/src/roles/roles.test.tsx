// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'

// Core
import { describe, expect, it, vi } from 'vitest'

// User interface
import { HeroUIProvider } from '@heroui/react'
import { MemberRolesPicker } from './MemberRolesPicker'
import { RoleLadder } from './RoleLadder'

// Utility
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

function role(id: string, position: number, overrides: Partial<Role> = {}): Role {
  return {
    id,
    key: null,
    name: id,
    color: '#000000',
    position,
    permissions: [],
    customized: true,
    isDefault: false,
    isAdministrator: false,
    ...overrides,
  }
}

/** Highest first: Administrator 4, Moderator 3, Billing 2, Editor 1, Member 0. */
const ladder: Role[] = [
  role('Administrator', 4, { isAdministrator: true }),
  role('Moderator', 3, { permissions: [ 'Tenancy::Membership.update' ]}),
  role('Billing', 2, { permissions: [ 'Billing::Subscription.update' ]}),
  role('Editor', 1, { permissions: [ 'Project.update' ]}),
  role('Member', 0, { isDefault: true }),
]

const moderator: TenantAccess = {
  roles: [{ id: 'Moderator', name: 'Moderator', color: '#000000', position: 3 }],
  permissions: [ 'Tenancy::Membership.update', 'Project.update' ],
  rank: 3,
  isAdministrator: false,
}

describe('MemberRolesPicker', () => {
  it('locks what the viewer may not hand out, and says why', () => {
    render(<HeroUIProvider>
      <MemberRolesPicker
        roles={ladder}
        viewer={moderator}
        value={[]}
        onChange={vi.fn()}
      />
    </HeroUIProvider>)

    expect(screen.queryByText('Member'), 'the default role is never offered').toBeNull()
    expect(screen.getByRole('checkbox', { name: /Editor/ })).toHaveProperty('disabled', false)
    expect(screen.getByRole('checkbox', { name: /Billing/ })).toHaveProperty('disabled', true)
    expect(screen.getByText('Carries permissions you do not hold')).toBeTruthy()
    expect(screen.getByRole('checkbox', { name: /Moderator/ })).toHaveProperty('disabled', true)
    expect(screen.getAllByText('Ranks at or above your highest role')).toHaveLength(2)
  })

  it('lets a held role below the viewer be taken away even when it carries more', async () => {
    const onChange = vi.fn()
    render(<HeroUIProvider>
      <MemberRolesPicker
        roles={ladder}
        viewer={moderator}
        value={[ 'Billing' ]}
        onChange={onChange}
      />
    </HeroUIProvider>)

    const billing = screen.getByRole('checkbox', { name: /Billing/ })
    expect(billing).toHaveProperty('disabled', false)
    await userEvent.click(billing)
    expect(onChange).toHaveBeenCalledWith([])
  })
})

describe('RoleLadder', () => {
  it('moves a role below the viewer and saves the whole order', async () => {
    const onReorder = vi.fn().mockResolvedValue(undefined)
    render(<HeroUIProvider>
      <RoleLadder
        roles={ladder}
        viewer={moderator}
        hrefFor={(entry) => `/roles/${entry.id}`}
        onReorder={onReorder}
      />
    </HeroUIProvider>)

    const moveUp = screen.getAllByRole('button', { name: 'Move up' })
    expect(moveUp, 'only Billing and Editor are below the moderator').toHaveLength(2)
    expect(moveUp[0], 'Billing would rise to the moderator').toHaveProperty('disabled', true)

    await userEvent.click(moveUp[1])
    expect(onReorder).toHaveBeenCalledWith([ 'Moderator', 'Editor', 'Billing' ])
  })
})
