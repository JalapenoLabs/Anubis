// Copyright © 2026 Jalapeno Labs

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
  ],
  server: {
    // `.env` names this origin in APP_URL, and the email links, OAuth redirect
    // URIs, and Stripe returns are all built from it. Silently moving to the
    // next free port would leave every one of them pointing at nothing, so a
    // taken port is an error worth seeing.
    strictPort: true,
    // Playwright writes its report and its failure artifacts into this
    // package. Watching them means a screenshot taken mid-run reloads the page
    // the run is driving, which is flake the application did not cause.
    watch: {
      ignored: [
        '**/playwright-report/**',
        '**/test-results/**',
      ],
    },
    // Every path the backend claims in production. In production one binary
    // serves both halves, so a path missing here works when deployed and
    // 404s in development, which is the worst way to find out. The list is
    // the backend's reserved prefixes plus its probes, and a test in the
    // framework fails when the two drift apart.
    proxy: {
      // The versioned public API, and the OpenAPI document and docs page under
      // it.
      '/api': 'http://127.0.0.1:3000',
      '/auth': 'http://127.0.0.1:3000',
      '/tenancy': 'http://127.0.0.1:3000',
      // The organization's plan, and the Stripe redirects that change it.
      '/billing': 'http://127.0.0.1:3000',
      '/account': 'http://127.0.0.1:3000',
      // Platform applications and outgoing webhook subscriptions.
      '/developers': 'http://127.0.0.1:3000',
      // Incoming webhooks, so a provider's test delivery can be pointed at a
      // tunnel to the dev server.
      '/webhooks': 'http://127.0.0.1:3000',
      // Avatars are served publicly, outside the authenticated prefixes.
      '/users': 'http://127.0.0.1:3000',
      '/healthz': 'http://127.0.0.1:3000',
      '/readyz': 'http://127.0.0.1:3000',
      // The realtime channel socket, which needs an explicit upgrade.
      '/realtime': {
        target: 'ws://127.0.0.1:3000',
        ws: true,
      },
    },
  },
})
