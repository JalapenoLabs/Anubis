// Copyright © 2026 Jalapeno Labs

import type { FieldOption } from './types'
import type { Key } from 'swr'

// Core
import useSWR from 'swr'

/** A stable empty list, so an unresolved fetch never re-renders the field. */
const NO_OPTIONS: FieldOption[] = []

/** The envelope every Anubis options endpoint answers with. */
type FieldOptionsResponse = {
  options: FieldOption[]
}

/**
 * The options a select field offers, fetched from an options endpoint.
 *
 * Both association scaffolds answer `{ "options": [{ value, label }] }`, which
 * is this package's `FieldOption` exactly, so a generated form hands the
 * result straight to its control. The identity of the returned array is stable
 * while a request is in flight, which keeps a field from re-rendering on every
 * parent render.
 *
 * @example
 * ```tsx
 * const leadOptions = useFieldOptions(
 *   [ 'project-lead-options', teamId ],
 *   () => listProjectLeadOptions(teamId),
 * )
 * ```
 */
export function useFieldOptions(
  key: Key,
  fetcher: () => Promise<FieldOptionsResponse>,
): FieldOption[] {
  const response = useSWR(key, fetcher)
  return response.data?.options ?? NO_OPTIONS
}
