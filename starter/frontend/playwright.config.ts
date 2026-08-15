// Copyright © 2026 Jalapeno Labs

import { defineConfig, devices } from '@playwright/test'

/**
 * The origin the specs drive, which is the Vite dev server in development.
 *
 * It comes from the environment because CI binds the same stack to its own
 * port, and because a deployed environment is a legitimate target for the same
 * specs.
 */
const baseURL = process.env.E2E_BASE_URL ?? 'http://localhost:5173'

/**
 * End-to-end configuration for the starter.
 *
 * There is deliberately no `webServer` block. A real run of this application is
 * three processes, Postgres, the Rust backend, and Vite, and Playwright can
 * only start one command; a block that started Vite alone would report a green
 * server while every request it proxies answered nothing. So the config expects
 * the stack to be running and says so: `yarn dev` locally, explicit steps in
 * CI. `e2e/global-setup.ts` waits for that stack and fails with instructions
 * when it never arrives, which is the whole of what a `webServer` block would
 * have bought.
 *
 * Chromium only. Cross-browser coverage is a project matrix, and adding one is
 * a decision about runner minutes rather than about test code: every spec here
 * is written against roles and labels, so the day the matrix grows they run
 * unchanged.
 */
export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.ts',
  // The specs write to a shared development database, so they run one at a
  // time. Each one signs up its own account, which keeps them independent, but
  // serial execution is what keeps a failure readable.
  workers: 1,
  fullyParallel: false,
  // A retry masks the flake it retries. Failures here are meant to be read.
  retries: 0,
  // Only ever pass locally by accident: `test.only` left in a spec is a CI
  // failure rather than a suite that quietly stopped covering anything.
  forbidOnly: Boolean(process.env.CI),
  reporter: [
    [ 'list' ],
    [ 'html', { open: 'never' }],
  ],
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'off',
  },
  projects: [
    {
      name: 'chromium',
      use: devices['Desktop Chrome'],
    },
  ],
})
