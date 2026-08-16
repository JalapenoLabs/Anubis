// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Utility
import { act, renderHook } from '@testing-library/react'

// Misc
import { useRetryCountdown } from './useRetryCountdown'

/** Advances the fake clock by whole seconds, flushing the renders each tick. */
async function tick(seconds: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(seconds * 1_000)
  })
}

describe('useRetryCountdown', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('should sit done until a wait starts it', () => {
    const { result } = renderHook(() => useRetryCountdown())

    expect(result.current.remaining).toBe(0)
    expect(result.current.done).toBe(true)
  })

  it('should count the wait down one second at a time and finish', async () => {
    const { result } = renderHook(() => useRetryCountdown())

    act(() => result.current.start(3))
    expect(result.current.remaining).toBe(3)
    expect(result.current.done).toBe(false)

    await tick(1)
    expect(result.current.remaining).toBe(2)

    await tick(1)
    expect(result.current.remaining).toBe(1)
    expect(result.current.done).toBe(false)

    await tick(1)
    expect(result.current.remaining).toBe(0)
    expect(result.current.done).toBe(true)

    // The interval stopped rather than counting past zero.
    await tick(5)
    expect(result.current.remaining).toBe(0)
  })

  it('should restart on a second refusal naming the same wait', async () => {
    const { result } = renderHook(() => useRetryCountdown())

    act(() => result.current.start(2))
    await tick(2)
    expect(result.current.done).toBe(true)

    act(() => result.current.start(2))
    expect(result.current.remaining).toBe(2)
    expect(result.current.done).toBe(false)

    await tick(1)
    expect(result.current.remaining).toBe(1)
  })

  it('should replace a running countdown rather than run two', async () => {
    const { result } = renderHook(() => useRetryCountdown())

    act(() => result.current.start(10))
    await tick(1)
    act(() => result.current.start(4))

    expect(result.current.remaining).toBe(4)

    // A leftover interval from the first countdown would double this tick.
    await tick(1)
    expect(result.current.remaining).toBe(3)
  })

  it('should ignore a wait that has nothing to wait for', () => {
    const { result } = renderHook(() => useRetryCountdown())

    act(() => result.current.start(0))

    expect(result.current.remaining).toBe(0)
    expect(result.current.done).toBe(true)
  })

  it('should stop ticking once the form unmounts', async () => {
    const { result, unmount } = renderHook(() => useRetryCountdown())

    act(() => result.current.start(5))
    unmount()

    await tick(5)
    expect(vi.getTimerCount()).toBe(0)
  })
})
