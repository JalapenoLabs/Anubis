// Copyright © 2026 Jalapeno Labs

import type { User } from './types'

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

  /**
   * The public URL serving a user's avatar, or `undefined` when there is none.
   *
   * The user's `avatarVersion` rides along as `?v=`, so the URL changes with
   * the picture: a fresh upload appears everywhere the moment the profile is
   * refetched, and the image itself stays cacheable for a year.
   *
   * An account with no picture has no URL rather than one that 404s. Handing an
   * `<img>` a URL that fails paints the browser's broken-image glyph over the
   * initials the fallback just drew, and costs a guaranteed 404 on every page
   * that renders the account. `undefined` leaves the element without a source,
   * which is what makes the fallback the only thing on screen.
   */
  function avatarUrl(user: Pick<User, 'id' | 'avatarVersion'>): string | undefined {
    if (!user.avatarVersion) {
      return undefined
    }

    return `${prefix}users/${user.id}/avatar?v=${user.avatarVersion}`
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
