// Copyright © 2026 Jalapeno Labs

// Core
import { useCallback, useEffect, useRef, useState } from 'react'

/** How often the remaining wait is re-rendered, in milliseconds. */
const TICK_INTERVAL = 1_000

/**
 * Counts a `Retry-After` wait down to zero, one second at a time.
 *
 * A rate-limited form has two jobs: tell the user how long the wait is, and
 * keep the submit disabled until it is over. Both read from `remaining`, which
 * this hook re-renders once a second, and from `done`, which is what a submit
 * button's `isDisabled` reads.
 *
 * Starting is a call rather than a prop because the input is an event, not a
 * value: two refusals in a row can name the same number of seconds, and a
 * countdown keyed on that number would never restart for the second one.
 *
 * ```ts
 * const retry = useRetryCountdown()
 *
 * catch (error) {
 *   const wait = getRetryAfterSeconds(error)
 *   if (wait) {
 *     retry.start(wait)
 *   }
 * }
 * ```
 */
export function useRetryCountdown() {
  const [ remaining, setRemaining ] = useState(0)
  const timer = useRef<ReturnType<typeof setInterval> | null>(null)

  // A countdown that outlives its form would set state on an unmounted tree.
  useEffect(() => {
    return () => {
      if (timer.current) {
        clearInterval(timer.current)
      }
    }
  }, [])

  const start = useCallback((seconds: number) => {
    if (seconds <= 0) {
      console.debug('useRetryCountdown was started with a non-positive wait', seconds)
      return
    }

    if (timer.current) {
      clearInterval(timer.current)
    }

    setRemaining(seconds)

    // The countdown lives in this closure rather than in the state updater, so
    // the interval clears itself exactly once however often React re-runs a
    // render.
    let left = seconds
    timer.current = setInterval(() => {
      left -= 1
      setRemaining(Math.max(left, 0))
      if (left <= 0 && timer.current) {
        clearInterval(timer.current)
        timer.current = null
      }
    }, TICK_INTERVAL)
  }, [])

  return {
    remaining,
    done: remaining === 0,
    start,
  } as const
}

export type RetryCountdown = ReturnType<typeof useRetryCountdown>
