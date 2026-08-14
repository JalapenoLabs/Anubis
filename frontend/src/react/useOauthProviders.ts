// Copyright © 2026 Jalapeno Labs

import type { OauthProvider } from '../api/types'

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

const OAUTH_PROVIDERS_KEY = 'anubis/oauth-providers'

/**
 * The OAuth providers this deployment can sign in with.
 *
 * A sign-in page renders one button per entry, so an unconfigured provider is
 * simply absent rather than a button that fails at click time. The list is a
 * property of the deployment's environment, so it changes only when the
 * backend restarts; SWR's cache is what keeps it to one request per page load.
 */
export function useOauthProviders() {
  const api = useAnubisApi()

  const { data, error, isLoading } = useSWR<OauthProvider[]>(
    OAUTH_PROVIDERS_KEY,
    () => api.listOauthProviders(),
  )

  return {
    providers: data ?? [],
    isLoading,
    error,
  } as const
}

export type OauthProvidersResult = ReturnType<typeof useOauthProviders>
