// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

// Core
import { MemoryRouter } from 'react-router'

// UI
import { HeroUIProvider } from '@heroui/react'

// Utility
import { render } from '@testing-library/react'
import { vi } from 'vitest'
import { SWRConfig } from 'swr'

// Misc
import { AnubisProvider } from '@jalapenolabs/anubis'
import { api } from '../api'
import '../i18n'

/** One stubbed response, keyed by the path the component under test requests. */
type StubbedResponse = {
  status: number
  body?: unknown
  /** Response headers beyond `content-type`, such as `Retry-After` on a 429. */
  headers?: Record<string, string>
}

/**
 * Renders `ui` inside the providers `main.tsx` mounts, at `initialEntry`.
 *
 * The SWR cache is fresh per render, so one test's `/auth/me` never answers
 * the next test's, which is the failure mode a shared module-level cache
 * produces once two tests fetch the same key.
 */
export function renderWithProviders(ui: ReactNode, initialEntry: string = '/') {
  // Built once per call rather than inside the provider callback: SWR asks for
  // the cache again on every re-render, and a fresh Map each time would throw
  // away the data that had just arrived.
  const cache = new Map()

  return render(
    <MemoryRouter initialEntries={[ initialEntry ]}>
      <HeroUIProvider>
        <AnubisProvider api={api}>
          <SWRConfig
            value={{
              provider: () => cache,
              dedupingInterval: 0,
            }}
          >{
              ui
            }</SWRConfig>
        </AnubisProvider>
      </HeroUIProvider>
    </MemoryRouter>,
  )
}

/**
 * Answers `fetch` from a table of paths, so a component meets a real client.
 *
 * The app's ky instance is the one `main.tsx` builds, so a test exercises the
 * same URL building, the same status handling, and the same wire shapes the
 * browser does. A path nobody listed answers 404 rather than hanging, which
 * turns a forgotten route into a failed assertion instead of a timeout.
 *
 * `vitest.setup.ts` restores the real `fetch` after every test.
 */
export function stubFetch(responses: Record<string, StubbedResponse>) {
  vi.stubGlobal('fetch', async (input: Request | string | URL) => {
    const requested = input instanceof Request
      ? input.url
      : String(input)
    const { pathname } = new URL(requested, 'http://localhost')

    const stubbed = responses[pathname]
    if (!stubbed) {
      console.debug('stubFetch has no response for', pathname)
      return new Response('{}', { status: 404 })
    }

    return new Response(JSON.stringify(stubbed.body ?? {}), {
      status: stubbed.status,
      headers: {
        'content-type': 'application/json',
        ...stubbed.headers,
      },
    })
  })
}
