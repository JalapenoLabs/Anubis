// Copyright © 2026 Jalapeno Labs

import type {
  ClaimedInvitation,
  CreatedOrganization,
  InviteMemberRequest,
  MemberAccountStatus,
  MembershipsOverview,
  OrganizationRosterMember,
  TeamRosterMember,
  TenancyOrganization,
  TenancyTeam,
} from '../types'
import type { KyInstance } from 'ky'

type WireRosterMember = {
  membership_id: string
  email: string | null
  roles: string[]
  pending: boolean
  invitation_id: string | null
}

type WireOrganizationRosterMember = {
  membership_id: string | null
  email: string
  roles: string[]
  pending: boolean
  invitation_id: string | null
}

type WireAccountStatus = {
  membership_id: string
  disabled: boolean
  password_change_required: boolean
}

type WireOrganization = {
  id: string
  name: string
}

type WireTeam = {
  id: string
  organization_id: string
  name: string
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
      invitationId: member.invitation_id,
    }))
  }

  async function listOrganizationMembers(
    organizationId: string,
  ): Promise<OrganizationRosterMember[]> {
    const response = await client
      .get(`tenancy/organizations/${organizationId}/members`)
      .json<{ members: WireOrganizationRosterMember[] }>()
    return response.members.map((member) => ({
      membershipId: member.membership_id,
      email: member.email,
      roles: member.roles,
      pending: member.pending,
      invitationId: member.invitation_id,
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

  /** Creates an organization; the backend gives it a default team. */
  async function createOrganization(name: string): Promise<CreatedOrganization> {
    const response = await client
      .post('tenancy/organizations', { json: { name }})
      .json<{ organization: WireOrganization, team: WireTeam }>()
    return {
      organization: {
        id: response.organization.id,
        name: response.organization.name,
      },
      team: {
        id: response.team.id,
        organizationId: response.team.organization_id,
        name: response.team.name,
      },
    }
  }

  async function renameOrganization(
    organizationId: string,
    name: string,
  ): Promise<TenancyOrganization> {
    const response = await client
      .patch(`tenancy/organizations/${organizationId}`, { json: { name }})
      .json<{ organization: WireOrganization }>()
    return {
      id: response.organization.id,
      name: response.organization.name,
    }
  }

  /** Deletes the organization, its teams, and everything that chains to them. */
  async function deleteOrganization(organizationId: string): Promise<void> {
    await client.delete(`tenancy/organizations/${organizationId}`)
  }

  async function leaveOrganization(organizationId: string): Promise<void> {
    await client.post(`tenancy/organizations/${organizationId}/leave`)
  }

  async function removeOrganizationMember(
    organizationId: string,
    membershipId: string,
  ): Promise<void> {
    await client.delete(`tenancy/organizations/${organizationId}/members/${membershipId}`)
  }

  async function revokeOrganizationInvitation(
    organizationId: string,
    invitationId: string,
  ): Promise<void> {
    await client.delete(`tenancy/organizations/${organizationId}/invitations/${invitationId}`)
  }

  /**
   * Disables a member's account, revoking every session it holds.
   *
   * Offboarding that keeps what the person wrote. The backend refuses this
   * aimed at your own account, and refuses one that would leave the
   * organization with no admin able to sign in.
   */
  async function disableOrganizationMember(
    organizationId: string,
    membershipId: string,
  ): Promise<MemberAccountStatus> {
    const response = await client
      .post(`tenancy/organizations/${organizationId}/members/${membershipId}/disable`)
      .json<WireAccountStatus>()
    return {
      membershipId: response.membership_id,
      disabled: response.disabled,
      passwordChangeRequired: response.password_change_required,
    }
  }

  /** Returns a disabled account to use; its revoked sessions stay revoked. */
  async function enableOrganizationMember(
    organizationId: string,
    membershipId: string,
  ): Promise<MemberAccountStatus> {
    const response = await client
      .post(`tenancy/organizations/${organizationId}/members/${membershipId}/enable`)
      .json<WireAccountStatus>()
    return {
      membershipId: response.membership_id,
      disabled: response.disabled,
      passwordChangeRequired: response.password_change_required,
    }
  }

  /** Makes a member choose a new password before they do anything else. */
  async function requireOrganizationMemberPasswordChange(
    organizationId: string,
    membershipId: string,
  ): Promise<MemberAccountStatus> {
    const path = `tenancy/organizations/${organizationId}/members/${membershipId}`
    const response = await client
      .post(`${path}/require-password-change`)
      .json<WireAccountStatus>()
    return {
      membershipId: response.membership_id,
      disabled: response.disabled,
      passwordChangeRequired: response.password_change_required,
    }
  }

  async function createTeam(organizationId: string, name: string): Promise<TenancyTeam> {
    const response = await client
      .post(`tenancy/organizations/${organizationId}/teams`, { json: { name }})
      .json<{ team: WireTeam }>()
    return {
      id: response.team.id,
      organizationId: response.team.organization_id,
      name: response.team.name,
    }
  }

  /** Dissolving a team is the organization's call, so it is nested under one. */
  async function deleteTeam(organizationId: string, teamId: string): Promise<void> {
    await client.delete(`tenancy/organizations/${organizationId}/teams/${teamId}`)
  }

  async function renameTeam(teamId: string, name: string): Promise<TenancyTeam> {
    const response = await client
      .patch(`tenancy/teams/${teamId}`, { json: { name }})
      .json<{ team: WireTeam }>()
    return {
      id: response.team.id,
      organizationId: response.team.organization_id,
      name: response.team.name,
    }
  }

  /** Replaces a member's roles wholesale; the request states the end state. */
  async function changeTeamMemberRoles(
    teamId: string,
    membershipId: string,
    roles: string[],
  ): Promise<string[]> {
    const response = await client
      .patch(`tenancy/teams/${teamId}/members/${membershipId}`, { json: { roles }})
      .json<{ membership_id: string, roles: string[] }>()
    return response.roles
  }

  async function removeTeamMember(teamId: string, membershipId: string): Promise<void> {
    await client.delete(`tenancy/teams/${teamId}/members/${membershipId}`)
  }

  async function leaveTeam(teamId: string): Promise<void> {
    await client.post(`tenancy/teams/${teamId}/leave`)
  }

  async function revokeTeamInvitation(teamId: string, invitationId: string): Promise<void> {
    await client.delete(`tenancy/teams/${teamId}/invitations/${invitationId}`)
  }

  return {
    listMemberships,
    listTeamMembers,
    listOrganizationMembers,
    inviteMember,
    claimInvitation,
    createOrganization,
    renameOrganization,
    deleteOrganization,
    leaveOrganization,
    removeOrganizationMember,
    revokeOrganizationInvitation,
    disableOrganizationMember,
    enableOrganizationMember,
    requireOrganizationMemberPasswordChange,
    createTeam,
    deleteTeam,
    renameTeam,
    changeTeamMemberRoles,
    removeTeamMember,
    leaveTeam,
    revokeTeamInvitation,
  } as const
}
