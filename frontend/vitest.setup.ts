// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach } from 'vitest'

// Utility
import { cleanup } from '@testing-library/react'

// Testing Library only unmounts automatically when Vitest runs with globals,
// and this package keeps its imports explicit.
afterEach(cleanup)
