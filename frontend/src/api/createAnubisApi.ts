// Copyright © 2026 Jalapeno Labs

// Utility
import ky from 'ky'

// Misc
import { createAccountRoutes } from './routes/accountRoutes'
import { createAuthRoutes } from './routes/authRoutes'
import { createBillingRoutes } from './routes/billingRoutes'
import { createTenancyRoutes } from './routes/tenancyRoutes'

type AnubisApiOptions = {
  /** Path or URL the framework routes are mounted under. Defaults to same-origin. */
  prefix?: string
}

/**
 * Builds the typed client for the framework's own endpoints.
 *
 * Sessions ride on HttpOnly cookies, so no tokens are handled here; the
 * browser sends the cookie automatically on same-origin requests. Route
 * functions are grouped by the surface they belong to, under `api/routes`.
 */
export function createAnubisApi(options: AnubisApiOptions = {}) {
  // A trailing slash makes every path below a plain suffix, whether the
  // application mounted the framework at the origin root or behind a path.
  const prefix = (options.prefix ?? '/').replace(/\/?$/, '/')

  const client = ky.create({
    prefix,
    // Auth outcomes (401, 409, 400) are modeled responses, not retryable
    // transport failures.
    retry: 0,
  })

  /** The `<img>` source for the pending TOTP enrollment's QR code. */
  function totpQrUrl(): string {
    return `${prefix}auth/mfa/totp/qr.svg`
  }

  /** The public URL serving a user's avatar, 404 until one is uploaded. */
  function avatarUrl(userId: string): string {
    return `${prefix}users/${userId}/avatar`
  }

  return {
    ...createAuthRoutes(client),
    ...createAccountRoutes(client),
    ...createTenancyRoutes(client),
    ...createBillingRoutes(client),
    totpQrUrl,
    avatarUrl,
  } as const
}

export type AnubisApi = ReturnType<typeof createAnubisApi>
