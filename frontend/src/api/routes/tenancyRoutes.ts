// Copyright © 2026 Jalapeno Labs

import type {
  ClaimedInvitation,
  InviteMemberRequest,
  MembershipsOverview,
  TeamRosterMember,
} from '../types'
import type { KyInstance } from 'ky'

type WireRosterMember = {
  membership_id: string
  email: string | null
  roles: string[]
  pending: boolean
}

/** Organizations, teams, memberships, and invitations. */
export function createTenancyRoutes(client: KyInstance) {
  function listMemberships() {
    return client
      .get('tenancy/memberships')
      .json<MembershipsOverview>()
  }

  async function listTeamMembers(teamId: string): Promise<TeamRosterMember[]> {
    const response = await client
      .get(`tenancy/teams/${teamId}/members`)
      .json<{ members: WireRosterMember[] }>()
    return response.members.map((member) => ({
      membershipId: member.membership_id,
      email: member.email,
      roles: member.roles,
      pending: member.pending,
    }))
  }

  function inviteMember(request: InviteMemberRequest) {
    return client
      .post('tenancy/invitations', {
        json: {
          email: request.email,
          team_id: request.teamId,
          organization_id: request.organizationId,
          roles: request.roles ?? [],
        },
      })
      .json<{ invitation: { id: string, email: string } }>()
  }

  function claimInvitation(token: string) {
    return client
      .post('tenancy/invitations/claim', { json: { token }})
      .json<ClaimedInvitation>()
  }

  return {
    listMemberships,
    listTeamMembers,
    inviteMember,
    claimInvitation,
  } as const
}
