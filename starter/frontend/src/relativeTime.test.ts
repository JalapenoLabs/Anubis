// Copyright © 2026 Jalapeno Labs

// Utility
import { describe, expect, it } from 'vitest'

// Misc
import { relativeTime } from './relativeTime'

/** The present every case below is measured against. */
const NOW = new Date('2026-08-20T12:00:00Z')

function ago(milliseconds: number): string {
  return new Date(NOW.getTime() - milliseconds).toISOString()
}

const SECOND = 1000
const MINUTE = 60 * SECOND
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

describe('relativeTime', () => {
  it('should pick the largest unit that fits', () => {
    expect(relativeTime(ago(30 * SECOND), NOW)).toBe('30 seconds ago')
    expect(relativeTime(ago(5 * MINUTE), NOW)).toBe('5 minutes ago')
    expect(relativeTime(ago(3 * HOUR), NOW)).toBe('3 hours ago')
    expect(relativeTime(ago(2 * DAY), NOW)).toBe('2 days ago')
    expect(relativeTime(ago(10 * DAY), NOW)).toBe('1 week ago')
    expect(relativeTime(ago(60 * DAY), NOW)).toBe('2 months ago')
    expect(relativeTime(ago(400 * DAY), NOW)).toBe('1 year ago')
  })

  it('should read an event written this instant as now, not as zero seconds', () => {
    expect(relativeTime(NOW.toISOString(), NOW)).toBe('now')
  })

  it('should render a future timestamp as the future', () => {
    const later = new Date(NOW.getTime() + 2 * HOUR).toISOString()

    expect(relativeTime(later, NOW)).toBe('in 2 hours')
  })

  it('should hand back a timestamp it cannot parse', () => {
    // The server sent something; showing it beats showing "Invalid Date".
    expect(relativeTime('not a timestamp', NOW)).toBe('not a timestamp')
  })
})
