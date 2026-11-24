// Copyright © 2026 Jalapeno Labs

/**
 * Wallet amounts, met at the display layer.
 *
 * The backend keeps every wallet amount in micro-units, millionths of the
 * currency's major unit, so a charge for one minute of usage never rounds to
 * zero. People read and type the major unit instead: dollars, yen. This module
 * is the one place the screens convert between the two.
 *
 * Formatting goes through `Intl` rather than i18next, because the symbol, the
 * separators, and how many decimals a currency has all belong to the locale
 * and the currency. No locale argument, matching every other amount on these
 * screens, which is the browser's.
 */

/** Micro-units in one major unit of any currency: one dollar, one yen. */
export const MICROS_PER_UNIT = 1_000_000

/** Why a typed amount cannot be charged. */
export type AmountProblem = 'missing' | 'belowMinimum' | 'partialUnit'

type FormatOptions = {
  /** Up to the micro-unit, for ledger lines where $0.0875 is a real charge. */
  precise?: boolean
  /** A leading `+` on credits, so they read apart from the debits beside them. */
  signed?: boolean
}

/**
 * Renders micro-units as money: `$10.00`, or `-$0.0875` when precise.
 *
 * A balance or a setting shows the currency's own decimals, which is what a
 * person thinks in. A ledger line shows up to six, because hiding a sub-cent
 * charge behind `$0.09` would make the column fail to add up.
 */
export function formatMicros(micros: number, currency: string, options: FormatOptions = {}): string {
  const formatter = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: currency.toUpperCase(),
    // Only the maximum moves. The minimum stays the currency's own, so a whole
    // amount still reads `$10.00`, and a zero-decimal currency never grows any.
    maximumFractionDigits: options.precise
      ? 6
      : undefined,
    signDisplay: options.signed
      ? 'exceptZero'
      : 'auto',
  })

  return formatter.format(micros / MICROS_PER_UNIT)
}

/** The currency's symbol as the reader's locale writes it, for an amount field: `$`. */
export function currencySymbol(currency: string): string {
  const code = currency.toUpperCase()
  const parts = new Intl.NumberFormat(undefined, { style: 'currency', currency: code })
    .formatToParts(0)

  return parts.find((part) => part.type === 'currency')?.value ?? code
}

/**
 * The smallest step Stripe charges in, in micro-units: one cent for usd.
 *
 * A currency read in whole units, jpy, or isk and ugx (which Stripe writes in
 * hundredths but charges only whole units of), charges whole units. Every
 * other one a wallet can hold charges hundredths. That is the backend's own
 * rule (`anubis::billing::currency::charge_step_micros`), and checking it here
 * only spares the person a refusal.
 */
export function chargeStepMicros(currency: string): number {
  const digits = new Intl.NumberFormat(undefined, { style: 'currency', currency: currency.toUpperCase() })
    .resolvedOptions()
    .maximumFractionDigits

  if (digits === 0) {
    return MICROS_PER_UNIT
  }

  return MICROS_PER_UNIT / 100
}

/**
 * Micro-units for an amount typed in the major unit, or null for no number.
 *
 * `Number` rather than `parseFloat`, which would read `10abc` as ten. Rounding
 * to the micro-unit absorbs binary noise (10.07 times a million is
 * 10070000.000000002) without hiding a typed fraction of a cent, which the
 * step check then refuses.
 */
export function parseMicros(typed: string): number | null {
  const trimmed = typed.trim()
  // An empty field is the field's own resting state rather than a mistake, and
  // `Number('')` would read it as zero.
  if (!trimmed) {
    return null
  }

  const amount = Number(trimmed)
  if (!Number.isFinite(amount)) {
    console.debug('parseMicros could not read an amount', typed)
    return null
  }

  return Math.round(amount * MICROS_PER_UNIT)
}

/**
 * Why `micros` cannot be charged with `minimumMicros` as its floor, or null when it can.
 *
 * The backend's checks, in the order a person fixes them: an amount at all,
 * then enough of it, then one Stripe can charge.
 */
export function findAmountProblem(
  micros: number | null,
  minimumMicros: number,
  currency: string,
): AmountProblem | null {
  if (micros === null) {
    return 'missing'
  }

  if (micros < minimumMicros) {
    return 'belowMinimum'
  }

  if (micros % chargeStepMicros(currency)) {
    return 'partialUnit'
  }

  return null
}
