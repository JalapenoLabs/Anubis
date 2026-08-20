// Copyright © 2026 Jalapeno Labs

import type { FieldOption } from './types'

// Core
import { describe, expect, it, vi } from 'vitest'

// Utility
import { renderHook, waitFor } from '@testing-library/react'

// Misc
import { useFieldOptions } from './useFieldOptions'

describe('useFieldOptions', () => {
  it('should answer a stable empty list until the options arrive', async () => {
    const options: FieldOption[] = [{ value: 'a-uuid', label: 'Ada Lovelace' }]
    const fetcher = vi.fn().mockResolvedValue({ options })

    const { result, rerender } = renderHook(
      () => useFieldOptions([ 'project-lead-options', 'a-team' ], fetcher),
    )

    const before = result.current
    expect(before).toEqual([])
    // The same array, not merely an equal one: a new one every render would
    // re-render the control on every parent render.
    rerender()
    expect(result.current).toBe(before)

    await waitFor(() => {
      expect(result.current).toEqual(options)
    })
  })

  it('should fetch nothing while its key is null', () => {
    const fetcher = vi.fn()

    const { result } = renderHook(() => useFieldOptions(null, fetcher))

    // A nested model's form renders before its parent record has loaded, so
    // the team it scopes to is briefly unknown.
    expect(result.current).toEqual([])
    expect(fetcher).not.toHaveBeenCalled()
  })
})
