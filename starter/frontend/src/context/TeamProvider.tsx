// Copyright © 2026 Jalapeno Labs

import type { MembershipOrganization, MembershipTeam, MembershipsResult } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { createContext, useContext, useMemo, useState } from 'react'
import { useMemberships } from '@jalapenolabs/anubis'

type CurrentTeam = {
  organization: MembershipOrganization
  team: MembershipTeam
}

type TeamContextValue = {
  memberships: MembershipsResult
  /** The selected team, or null while memberships load or when there is none. */
  current: CurrentTeam | null
  selectTeam: (teamId: string) => void
}

const STORAGE_KEY = 'anubis.currentTeamId'

const TeamContext = createContext<TeamContextValue | null>(null)

type Props = {
  children: ReactNode
}

/** Tracks which team the user is working in, persisted across reloads. */
export function TeamProvider(props: Props) {
  const memberships = useMemberships()
  const [ selectedTeamId, setSelectedTeamId ] = useState<string | null>(() => {
    return window.localStorage.getItem(STORAGE_KEY)
  })

  const current = useMemo(
    () => resolveCurrentTeam(memberships.organizations, selectedTeamId),
    [ memberships.organizations, selectedTeamId ],
  )

  const value: TeamContextValue = {
    memberships,
    current,
    selectTeam: (teamId) => {
      window.localStorage.setItem(STORAGE_KEY, teamId)
      setSelectedTeamId(teamId)
    },
  }

  return <TeamContext.Provider value={value}>{
      props.children
    }</TeamContext.Provider>
}

/** Returns the team context; requires a TeamProvider above in the tree. */
export function useTeamContext(): TeamContextValue {
  const context = useContext(TeamContext)
  if (!context) {
    throw new Error('useTeamContext requires a <TeamProvider> above it in the tree')
  }
  return context
}

/** Finds the selected team, falling back to the first team anywhere. */
export function resolveCurrentTeam(
  organizations: MembershipOrganization[],
  selectedTeamId: string | null,
): CurrentTeam | null {
  let fallback: CurrentTeam | null = null

  for (const organization of organizations) {
    for (const team of organization.teams) {
      if (team.id === selectedTeamId) {
        return {
          organization,
          team,
        }
      }
      if (!fallback) {
        fallback = {
          organization,
          team,
        }
      }
    }
  }

  return fallback
}
