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
    proxy: {
      // The backend serves the API; the SPA stays same-origin in production.
      '/auth': 'http://127.0.0.1:3000',
      '/tenancy': 'http://127.0.0.1:3000',
      '/account': 'http://127.0.0.1:3000',
      // Avatars are served publicly, outside the authenticated prefixes.
      '/users': 'http://127.0.0.1:3000',
      '/healthz': 'http://127.0.0.1:3000',
    },
  },
})
