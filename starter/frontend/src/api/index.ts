// Copyright © 2026 Jalapeno Labs

// Utility
import ky from 'ky'

// Misc
import { createAnubisApi } from '@jalapenolabs/anubis'

// The vite dev server proxies the backend routes; in production the backend
// serves the SPA, so same-origin works everywhere.
export const api = createAnubisApi()

/**
 * The client for the application's own endpoints, mounted under `/account`.
 *
 * Sessions ride on the HttpOnly cookie the framework sets, so no token is
 * handled here. Route functions live in `api/routes`, one module per model,
 * and `anubis scaffold model` writes a new one per model it generates.
 */
export const appClient = ky.create({
  prefix: '/account/',
  // Application outcomes (403, 404, 400) are modeled responses, not retryable
  // transport failures.
  retry: 0,
})

/** The pagination object every list endpoint returns beside its records. */
export type Pagination = {
  page: number
  limit: number
  total_items: number
  total_pages: number
}

/** The query every list endpoint accepts; see `docs/api.md`. */
export type ListQuery = {
  page?: number
  limit?: number
  /** A whitelisted field name, prefixed with `-` for descending. */
  sort?: string
}

/** Drops absent entries so the request URL carries only what was asked for. */
export function toSearchParams(query: Record<string, string | number | undefined>) {
  const searchParams: Record<string, string> = {}
  for (const [ key, value ] of Object.entries(query)) {
    if (value !== undefined && value !== '') {
      searchParams[key] = String(value)
    }
  }
  return searchParams
}
