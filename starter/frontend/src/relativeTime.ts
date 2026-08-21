// Copyright © 2026 Jalapeno Labs

/**
 * How long ago something happened, in the reader's own language.
 *
 * The audit log is read by scanning, and "3 hours ago" answers the scanning
 * question that a timestamp makes you compute. The exact moment still belongs
 * on the screen, as the tooltip beside it.
 *
 * Formatting goes through `Intl.RelativeTimeFormat` rather than through
 * i18next: the phrasing, the pluralization, and the word order all belong to
 * the locale, and baking any of them into a translation key would put them in
 * the wrong place. No locale argument, matching every other date on these
 * screens, which is the browser's.
 */

/** The units this renders in, largest first, with their length in seconds. */
const UNITS = [
  { unit: 'year', seconds: 60 * 60 * 24 * 365 },
  { unit: 'month', seconds: 60 * 60 * 24 * 30 },
  { unit: 'week', seconds: 60 * 60 * 24 * 7 },
  { unit: 'day', seconds: 60 * 60 * 24 },
  { unit: 'hour', seconds: 60 * 60 },
  { unit: 'minute', seconds: 60 },
  { unit: 'second', seconds: 1 },
] as const satisfies readonly { unit: Intl.RelativeTimeFormatUnit, seconds: number }[]

/**
 * Renders an ISO 8601 timestamp as a phrase like "3 hours ago".
 *
 * `now` is injectable so a test can state its own present; callers pass
 * nothing. A timestamp the browser cannot parse comes back verbatim, since
 * showing what the server sent beats showing "Invalid Date".
 */
export function relativeTime(timestamp: string, now: Date = new Date()): string {
  const moment = new Date(timestamp)
  if (Number.isNaN(moment.getTime())) {
    console.debug('relativeTime received an unparseable timestamp', timestamp)
    return timestamp
  }

  const elapsedSeconds = (moment.getTime() - now.getTime()) / 1000
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'always' })

  for (const { unit, seconds } of UNITS) {
    if (Math.abs(elapsedSeconds) >= seconds) {
      return formatter.format(Math.round(elapsedSeconds / seconds), unit)
    }
  }

  // Under a second either way. `numeric: 'auto'` is what renders that as
  // "now"; the formatter above would call a row written this instant "in 0
  // seconds", which reads as the future and is wrong in both halves.
  return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(0, 'second')
}
