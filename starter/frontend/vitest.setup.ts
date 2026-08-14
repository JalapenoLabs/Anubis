// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, vi } from 'vitest'

// Utility
import { cleanup } from '@testing-library/react'

// Testing Library only unmounts automatically when Vitest runs with globals,
// and this app keeps its imports explicit.
afterEach(cleanup)

// `stubFetch` replaces the global fetch; restoring it here keeps one test's
// stubbed backend out of the next one.
afterEach(() => {
  vi.unstubAllGlobals()
})
