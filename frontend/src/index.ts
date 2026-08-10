// Copyright © 2026 Jalapeno Labs

export type {
  Credentials,
  MessageEnvelope,
  User,
  UserEnvelope,
} from './api/types'
export type { AnubisApi } from './api/createAnubisApi'
export type { CurrentUserResult } from './react/useCurrentUser'

// Misc
export { createAnubisApi } from './api/createAnubisApi'
export { getApiErrorMessage } from './api/errors'
export { AnubisProvider, useAnubisApi } from './react/AnubisProvider'
export { useCurrentUser } from './react/useCurrentUser'

/**
 * The version of the Anubis frontend package.
 *
 * Kept in lockstep with package.json; the unit test guards the pairing.
 */
export const ANUBIS_VERSION = '0.1.0'
