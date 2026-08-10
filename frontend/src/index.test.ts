// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, expectTypeOf, it } from 'vitest'

// Misc
import packageJson from '../package.json'
import { ANUBIS_VERSION } from './index'

describe('ANUBIS_VERSION', () => {
  it('should match the package.json version', () => {
    expect(ANUBIS_VERSION).toBe(packageJson.version)
  })

  it('should be MAJOR.MINOR.PATCH shaped', () => {
    expect(ANUBIS_VERSION).toMatch(/^\d+\.\d+\.\d+$/)
  })

  it('should be a string constant', () => {
    expectTypeOf(ANUBIS_VERSION).toBeString()
  })
})
