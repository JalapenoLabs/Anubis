// Copyright © 2026 Jalapeno Labs

import type { AmountProblem } from './money'

// Core
import { describe, expect, expectTypeOf, it } from 'vitest'

// Misc
import {
  MICROS_PER_UNIT,
  chargeStepMicros,
  currencySymbol,
  findAmountProblem,
  formatMicros,
  parseMicros,
} from './money'

describe('formatMicros', () => {
  it('should show a balance in the decimals of its currency', () => {
    expect(formatMicros(10_000_000, 'usd')).toBe('$10.00')
    expect(formatMicros(4_912_500, 'usd')).toBe('$4.91')
  })

  it('should keep a sub-cent ledger charge visible when precise', () => {
    expect(formatMicros(-87_500, 'usd', { precise: true })).toBe('-$0.0875')
    expect(formatMicros(1, 'usd', { precise: true })).toBe('$0.000001')
  })

  it('should still write a whole amount with its cents when precise', () => {
    expect(formatMicros(25_000_000, 'usd', { precise: true })).toBe('$25.00')
  })

  it('should mark credits with a plus only when signed', () => {
    expect(formatMicros(25_000_000, 'usd', { precise: true, signed: true })).toBe('+$25.00')
    expect(formatMicros(-5_000_000, 'usd', { precise: true, signed: true })).toBe('-$5.00')
    expect(formatMicros(0, 'usd', { signed: true })).toBe('$0.00')
  })

  it('should never give a zero-decimal currency any cents', () => {
    expect(formatMicros(1_000_000_000, 'jpy')).toBe('¥1,000')
  })
})

describe('currencySymbol', () => {
  it('should name the symbol an amount field starts with', () => {
    expect(currencySymbol('usd')).toBe('$')
    expect(currencySymbol('eur')).toBe('€')
  })
})

describe('chargeStepMicros', () => {
  it('should charge in cents and in whole yen', () => {
    expect(chargeStepMicros('usd')).toBe(10_000)
    expect(chargeStepMicros('jpy')).toBe(MICROS_PER_UNIT)
  })

  it('should charge whole krónur and shillings, which Stripe writes in hundredths', () => {
    expect(chargeStepMicros('isk')).toBe(MICROS_PER_UNIT)
    expect(chargeStepMicros('ugx')).toBe(MICROS_PER_UNIT)
  })
})

describe('parseMicros', () => {
  it('should read dollars as micro-units', () => {
    expect(parseMicros('10')).toBe(10_000_000)
    expect(parseMicros(' 12.34 ')).toBe(12_340_000)
  })

  it('should round away the binary noise of a decimal amount', () => {
    // 10.07 * 1e6 is 10070000.000000002 in floating point.
    expect(parseMicros('10.07')).toBe(10_070_000)
  })

  it('should keep a typed fraction of a cent for the step check to refuse', () => {
    expect(parseMicros('10.005')).toBe(10_005_000)
  })

  it('should answer null for an empty field or for text that is no number', () => {
    expect(parseMicros('')).toBeNull()
    expect(parseMicros('   ')).toBeNull()
    expect(parseMicros('10abc')).toBeNull()
    expect(parseMicros('Infinity')).toBeNull()
  })
})

describe('findAmountProblem', () => {
  it('should accept a whole-cent amount at the minimum or above', () => {
    expect(findAmountProblem(10_000_000, 10_000_000, 'usd')).toBeNull()
    expect(findAmountProblem(12_340_000, 10_000_000, 'usd')).toBeNull()
  })

  it('should ask for an amount before anything else', () => {
    expect(findAmountProblem(null, 10_000_000, 'usd')).toBe('missing')
  })

  it('should refuse an amount below the floor, a negative one included', () => {
    expect(findAmountProblem(9_990_000, 10_000_000, 'usd')).toBe('belowMinimum')
    expect(findAmountProblem(-1_000_000, 0, 'usd')).toBe('belowMinimum')
  })

  it('should refuse a fraction of what Stripe charges in', () => {
    expect(findAmountProblem(10_005_000, 10_000_000, 'usd')).toBe('partialUnit')
    // Half a yen, in a currency Stripe charges in whole units.
    expect(findAmountProblem(1_000_500_000, 1_000_000_000, 'jpy')).toBe('partialUnit')
  })

  it('should answer one of the named problems', () => {
    expectTypeOf(findAmountProblem(null, 0, 'usd')).toEqualTypeOf<AmountProblem | null>()
  })
})
