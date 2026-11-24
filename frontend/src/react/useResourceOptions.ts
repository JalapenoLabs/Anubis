// Copyright © 2026 Jalapeno Labs

// Core
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'

/** A stable empty list, so an unresolved fetch never re-renders the picker. */
const NO_OPTIONS: { value: string, label: string }[] = []

/**
 * The organization's records of one kind a sharing picker offers.
 *
 * `search` is what the person is typing, already debounced by the picker;
 * each query is its own request, so typing narrows a kind with thousands of
 * records to the few that match. Pass `isEnabled: false` to fetch nothing,
 * for a viewer who may not share the kind and so is refused the list.
 */
export function useResourceOptions(
  organizationId: string,
  kind: string,
  search: string,
  isEnabled: boolean,
) {
  const api = useAnubisApi()
  const key = isEnabled
    ? [ 'anubis/resource-options', organizationId, kind, search ]
    : null

  const { data, isLoading } = useSWR(key, () => api.listResourceOptions(
    organizationId,
    kind,
    search,
  ))

  return {
    options: data ?? NO_OPTIONS,
    isLoading,
  } as const
}

export type ResourceOptionsResult = ReturnType<typeof useResourceOptions>
