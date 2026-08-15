// Copyright © 2026 Jalapeno Labs

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

const REGISTRATION_KEY = 'anubis/registration'

/**
 * Whether this deployment accepts new accounts.
 *
 * A sign-up affordance reads it and stays off screen when the answer is
 * `false`, so a deployment that registers nobody never shows a form whose
 * every submission is refused. The answer is a property of the deployment's
 * environment, so it changes only when the backend restarts; SWR's cache is
 * what keeps it to one request per page load.
 *
 * While the request is in flight, and if it fails, the answer is `true`:
 * hiding sign-up on an open deployment because one discovery request failed is
 * the worse of the two mistakes, and the server refuses a closed registration
 * with 403 either way.
 */
export function useRegistrationOpen() {
  const api = useAnubisApi()

  const { data, error, isLoading } = useSWR<boolean>(
    REGISTRATION_KEY,
    () => api.isRegistrationOpen(),
  )

  return {
    isOpen: data ?? true,
    isLoading,
    error,
  } as const
}

export type RegistrationOpenResult = ReturnType<typeof useRegistrationOpen>
