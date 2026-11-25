// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantDraft } from './draft'

// Core
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'

// User interface
import { HeroUIProvider } from '@heroui/react'
import { ResourceGrantEditor } from './ResourceGrantEditor'

// Utility
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const rack = { value: 'rack', label: 'Rack' }
const tower = { value: 'tower', label: 'Tower' }

const labels = {
  group: 'Team C machines',
  every: 'Every machine',
  add: 'Add a machine',
}

type HarnessProps = {
  initial: ResourceGrantDraft
  onChange: (draft: ResourceGrantDraft) => void
}

/** Holds the draft the way a page does, so a pick redraws the editor. */
function Harness(props: HarnessProps) {
  const [ draft, setDraft ] = useState(props.initial)
  return <HeroUIProvider>
    <ResourceGrantEditor
      draft={draft}
      onChange={(next) => {
        setDraft(next)
        props.onChange(next)
      }}
      options={[ rack, tower ]}
      onSearch={vi.fn()}
      labels={labels}
    />
  </HeroUIProvider>
}

describe('ResourceGrantEditor', () => {
  it('offers the three end states a team can be in', () => {
    render(<Harness initial={{ mode: 'nothing', resources: []}} onChange={vi.fn()} />)

    expect(screen.getByRole('radio', { name: 'Nothing' })).toHaveProperty('checked', true)
    expect(screen.getByRole('radio', { name: 'Every machine' })).toHaveProperty('checked', false)
    expect(screen.getByRole('radio', { name: 'Only these' })).toHaveProperty('checked', false)
    expect(screen.queryByRole('combobox')).toBeNull()
  })

  it('picks records by name once only these is chosen', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(<Harness initial={{ mode: 'nothing', resources: []}} onChange={onChange} />)

    await user.click(screen.getByRole('radio', { name: 'Only these' }))
    expect(onChange).toHaveBeenLastCalledWith({ mode: 'only', resources: []})

    await user.click(screen.getByRole('combobox', { name: 'Add a machine' }))
    await user.click(await screen.findByRole('option', { name: 'Tower' }))
    expect(onChange).toHaveBeenLastCalledWith({ mode: 'only', resources: [ tower ]})
    expect(screen.getByText('Tower')).toBeTruthy()
  })

  it('keeps the picks when every is tried and taken back', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(<Harness initial={{ mode: 'only', resources: [ rack ]}} onChange={onChange} />)

    await user.click(screen.getByRole('radio', { name: 'Every machine' }))
    await user.click(screen.getByRole('radio', { name: 'Only these' }))
    expect(onChange).toHaveBeenLastCalledWith({ mode: 'only', resources: [ rack ]})
  })

  it('locks the choice and says why for a viewer who may not change it', () => {
    render(<HeroUIProvider>
      <ResourceGrantEditor
        draft={{ mode: 'every', resources: []}}
        onChange={vi.fn()}
        options={[]}
        onSearch={vi.fn()}
        isDisabled
        reason='Sharing machines takes every machine permission.'
        labels={labels}
      />
    </HeroUIProvider>)

    expect(screen.getByRole('radio', { name: 'Every machine' })).toHaveProperty('disabled', true)
    expect(screen.getByText('Sharing machines takes every machine permission.')).toBeTruthy()
  })
})
