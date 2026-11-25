// Copyright © 2026 Jalapeno Labs

import type { ResourceGrant } from '../api/types'
import type { ResourceGrantDraft } from './draft'

// Core
import { describe, expect, expectTypeOf, it } from 'vitest'

// Misc
import { changeOf, draftOf, isSameGrant } from './draft'

const rack = { value: 'rack', label: 'Rack' }
const tower = { value: 'tower', label: 'Tower' }

function grant(overrides: Partial<ResourceGrant> = {}): ResourceGrant {
  return {
    teamId: 'team',
    kind: 'Machine',
    every: false,
    resources: [],
    ...overrides,
  }
}

describe('draftOf', () => {
  it('reads no grant as nothing', () => {
    expect(draftOf(undefined)).toEqual({ mode: 'nothing', resources: []})
  })

  it('reads a grant of every record as every', () => {
    expect(draftOf(grant({ every: true }))).toEqual({ mode: 'every', resources: []})
  })

  it('keeps the names of the records a grant picked', () => {
    expect(draftOf(grant({ resources: [ rack ]}))).toEqual({ mode: 'only', resources: [ rack ]})
  })

  it('reads a grant naming no record as nothing', () => {
    expect(draftOf(grant()).mode).toBe('nothing')
  })
})

describe('changeOf', () => {
  it('sends every record and no ids for every', () => {
    const draft: ResourceGrantDraft = { mode: 'every', resources: [ rack ]}
    expect(changeOf(draft)).toEqual({ every: true, resourceIds: []})
  })

  it('sends the picked ids for only these', () => {
    expect(changeOf({ mode: 'only', resources: [ rack, tower ]})).toEqual({
      every: false,
      resourceIds: [ 'rack', 'tower' ],
    })
  })

  it('sends nothing for nothing, whatever was picked before', () => {
    expect(changeOf({ mode: 'nothing', resources: [ rack ]})).toEqual({
      every: false,
      resourceIds: [],
    })
  })

  it('types the change the server reads', () => {
    expectTypeOf(changeOf).returns.toEqualTypeOf<{ every: boolean, resourceIds: string[] }>()
  })
})

describe('isSameGrant', () => {
  it('ignores the order records were picked in', () => {
    expect(isSameGrant(
      { mode: 'only', resources: [ rack, tower ]},
      { mode: 'only', resources: [ tower, rack ]},
    )).toBe(true)
  })

  it('reads only these with nothing picked as nothing', () => {
    expect(isSameGrant(
      { mode: 'only', resources: []},
      { mode: 'nothing', resources: []},
    )).toBe(true)
  })

  it('tells every apart from a list, and one list from another', () => {
    expect(isSameGrant({ mode: 'every', resources: []}, { mode: 'only', resources: [ rack ]})).toBe(false)
    expect(isSameGrant(
      { mode: 'only', resources: [ rack ]},
      { mode: 'only', resources: [ tower ]},
    )).toBe(false)
  })
})
