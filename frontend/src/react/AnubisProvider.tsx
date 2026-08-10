// Copyright © 2026 Jalapeno Labs

import type { AnubisApi } from '../api/createAnubisApi'
import type { ReactNode } from 'react'

// Core
import { createContext, useContext } from 'react'

type Props = {
  api: AnubisApi
  children: ReactNode
}

const AnubisApiContext = createContext<AnubisApi | null>(null)

/**
 * Provides the Anubis API client to the component tree.
 *
 * Mount once near the root, above anything using framework hooks.
 */
export function AnubisProvider(props: Props) {
  return <AnubisApiContext.Provider value={props.api}>{
      props.children
    }</AnubisApiContext.Provider>
}

/** Returns the API client provided by the nearest AnubisProvider. */
export function useAnubisApi(): AnubisApi {
  const api = useContext(AnubisApiContext)
  if (!api) {
    throw new Error('useAnubisApi requires an <AnubisProvider> above it in the tree')
  }
  return api
}
