// Copyright © 2026 Jalapeno Labs

import type { ResourceGrant, ResourceGrantChange, ResourceGrantsOverview } from '../types'
import type { KyInstance } from 'ky'

type WireResourceGrant = {
  team_id: string
  kind: string
  every: boolean
  resources: { value: string, label: string }[]
}

type WireResourceGrantsOverview = {
  kinds: { kind: string, reaches: boolean, may_manage: boolean }[]
  teams: { id: string, name: string }[]
  grants: WireResourceGrant[]
}

function toGrant(wire: WireResourceGrant): ResourceGrant {
  return {
    teamId: wire.team_id,
    kind: wire.kind,
    every: wire.every,
    resources: wire.resources,
  }
}

/** Which teams reach the records an organization owns directly. */
export function createResourceGrantRoutes(client: KyInstance) {
  /** The kinds the application shares, the teams, and each team's grants. */
  async function listResourceGrants(organizationId: string): Promise<ResourceGrantsOverview> {
    const response = await client
      .get(`tenancy/organizations/${organizationId}/resource-grants`)
      .json<WireResourceGrantsOverview>()
    return {
      kinds: response.kinds.map((kind) => ({
        kind: kind.kind,
        reaches: kind.reaches,
        mayManage: kind.may_manage,
      })),
      teams: response.teams,
      grants: response.grants.map(toGrant),
    }
  }

  /**
   * Replaces what one team reaches of one kind with `change`, the whole end
   * state, so two managers can never interleave into a set neither asked for.
   */
  async function replaceResourceGrants(
    organizationId: string,
    teamId: string,
    kind: string,
    change: ResourceGrantChange,
  ): Promise<ResourceGrant> {
    const response = await client
      .put(`tenancy/organizations/${organizationId}/teams/${teamId}/resource-grants/${kind}`, {
        json: { every: change.every, resource_ids: change.resourceIds },
      })
      .json<{ grant: WireResourceGrant }>()
    return toGrant(response.grant)
  }

  /** The organization's records of a kind a picker offers, narrowed by `search`. */
  async function listResourceOptions(
    organizationId: string,
    kind: string,
    search?: string,
  ): Promise<{ value: string, label: string }[]> {
    const searchParams: Record<string, string> = {}
    if (search) {
      searchParams.search = search
    }
    const response = await client
      .get(`tenancy/organizations/${organizationId}/resource-grants/${kind}/options`, {
        searchParams,
      })
      .json<{ options: { value: string, label: string }[] }>()
    return response.options
  }

  return {
    listResourceGrants,
    replaceResourceGrants,
    listResourceOptions,
  } as const
}
