// Copyright © 2026 Jalapeno Labs

import type { FullConfig } from '@playwright/test'

/** How long the stack is given to come up before the run is abandoned. */
const STARTUP_TIMEOUT_MS = 180_000

/** How often the probe is retried while the stack is still starting. */
const PROBE_INTERVAL_MS = 500

/**
 * Waits for the whole application to answer before any spec runs.
 *
 * The probe is `GET /healthz` through the base URL, which is one request that
 * proves both halves: Vite is serving, and the backend it proxies to is up and
 * connected to its database. A spec that started before that answered would
 * fail on a connection error and blame the application.
 *
 * The wait is generous because the same command that starts the stack starts
 * this run, and a cold `cargo run` compiles the backend first.
 */
export default async function waitForTheStack(config: FullConfig): Promise<void> {
  const baseURL = config.projects[0]?.use.baseURL
  if (!baseURL) {
    throw new Error('playwright.config.ts must set a baseURL for the end-to-end suite')
  }

  const healthUrl = new URL('/healthz', baseURL)
  const deadline = Date.now() + STARTUP_TIMEOUT_MS
  let lastFailure = 'no attempt was made'

  while (Date.now() < deadline) {
    try {
      const response = await fetch(healthUrl)
      if (response.ok) {
        return
      }
      lastFailure = `${healthUrl} answered ${response.status}`
    }
    catch (error) {
      lastFailure = error instanceof Error ? error.message : String(error)
    }

    await new Promise((resolve) => setTimeout(resolve, PROBE_INTERVAL_MS))
  }

  throw new Error(
    `The application never answered at ${baseURL} (${lastFailure}).\n`
    + 'The end-to-end suite drives a real stack: Postgres, the backend, and Vite.\n'
    + 'Start it with `yarn dev` from the repository root, then run `yarn test:e2e` here,\n'
    + 'or run `yarn e2e` from the root to do both at once.',
  )
}
