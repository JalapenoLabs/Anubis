// Copyright © 2026 Jalapeno Labs

// Misc
import { createAnubisApi } from '@jalapenolabs/anubis'

// The vite dev server proxies /auth to the backend; in production the
// backend serves the SPA, so same-origin works everywhere.
export const api = createAnubisApi()
