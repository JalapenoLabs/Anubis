// Copyright © 2026 Jalapeno Labs

import type {
  ClaimedInvitation,
  CreatedOrganization,
  InviteMemberRequest,
  MembershipsOverview,
  OrganizationRosterMember,
  Role,
  RoleCatalog,
  RoleDraft,
  RoleSummary,
  RoleUpdate,
  TeamRosterMember,
  TenancyOrganization,
  TenancyTeam,
  TenantAccess,
} from '../types'
import type { KyInstance } from 'ky'

type WireAccess = {
  roles: RoleSummary[]
  permissions: string[]
  rank: number
  is_administrator: boolean
}

type WireMembershipTeam = WireAccess & {
  id: string
  name: string
}

type WireMembershipOrganization = WireAccess & {
  id: string
  name: string
  is_member: boolean
  teams: WireMembershipTeam[]
}

type WireRosterMember = {
  membership_id: string
  email: string | null
  roles: RoleSummary[]
  resource_roles: { resource_id: string, roles: RoleSummary[] }[]
  pending: boolean
  invitation_id: string | null
}

type WireOrganizationRosterMember = {
  membership_id: string | null
  email: string
  roles: RoleSummary[]
  pending: boolean
  invitation_id: string | null
}

type WireRole = {
  id: string
  key: string | null
  name: string
  color: string
  position: number
  permissions: string[]
  customized: boolean
  is_default: boolean
  is_administrator: boolean
}

type WireRoleCatalog = {
  roles: WireRole[]
  permissions: string[]
  viewer: WireAccess
}

function toAccess(wire: WireAccess): TenantAccess {
  return {
    roles: wire.roles,
    permissions: wire.permissions,
    rank: wire.rank,
    isAdministrator: wire.is_administrator,
  }
}

function toRole(wire: WireRole): Role {
  return {
    id: wire.id,
    key: wire.key,
    name: wire.name,
    color: wire.color,
    position: wire.position,
    permissions: wire.permissions,
    customized: wire.customized,
    isDefault: wire.is_default,
    isAdministrator: wire.is_administrator,
  }
}

function toCatalog(wire: WireRoleCatalog): RoleCatalog {
  return {
    roles: wire.roles.map(toRole),
    permissions: wire.permissions,
    viewer: toAccess(wire.viewer),
  }
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
  async function listMemberships(): Promise<MembershipsOverview> {
    const response = await client
      .get('tenancy/memberships')
      .json<{ organizations: WireMembershipOrganization[] }>()
    return {
      organizations: response.organizations.map((organization) => ({
        id: organization.id,
        name: organization.name,
        isMember: organization.is_member,
        access: toAccess(organization),
        teams: organization.teams.map((team) => ({
          id: team.id,
          name: team.name,
          access: toAccess(team),
        })),
      })),
    }
  }

  async function listTeamMembers(teamId: string): Promise<TeamRosterMember[]> {
    const response = await client
      .get(`tenancy/teams/${teamId}/members`)
      .json<{ members: WireRosterMember[] }>()
    return response.members.map((member) => ({
      membershipId: member.membership_id,
      email: member.email,
      roles: member.roles,
      resourceRoles: member.resource_roles.map((granted) => ({
        resourceId: granted.resource_id,
        roles: granted.roles,
      })),
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
          role_ids: request.roleIds ?? [],
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

  /**
   * Replaces a member's roles in the team wholesale; the request states the
   * end state. With a `resourceId`, the roles replaced are the ones on that
   * one record of the team.
   */
  async function changeTeamMemberRoles(
    teamId: string,
    membershipId: string,
    roleIds: string[],
    resourceId?: string,
  ): Promise<RoleSummary[]> {
    const response = await client
      .patch(`tenancy/teams/${teamId}/members/${membershipId}`, {
        json: { role_ids: roleIds, resource_id: resourceId },
      })
      .json<{ membership_id: string, roles: RoleSummary[] }>()
    return response.roles
  }

  /** Replaces a member's organization-wide roles wholesale. */
  async function changeOrganizationMemberRoles(
    organizationId: string,
    membershipId: string,
    roleIds: string[],
  ): Promise<RoleSummary[]> {
    const response = await client
      .patch(`tenancy/organizations/${organizationId}/members/${membershipId}`, {
        json: { role_ids: roleIds },
      })
      .json<{ membership_id: string, roles: RoleSummary[] }>()
    return response.roles
  }

  /** The organization's roles, with the viewer's organization-wide standing. */
  async function listOrganizationRoles(organizationId: string): Promise<RoleCatalog> {
    const response = await client
      .get(`tenancy/organizations/${organizationId}/roles`)
      .json<WireRoleCatalog>()
    return toCatalog(response)
  }

  /** The team organization's roles, with the viewer's standing in the team. */
  async function listTeamRoles(teamId: string): Promise<RoleCatalog> {
    const response = await client
      .get(`tenancy/teams/${teamId}/roles`)
      .json<WireRoleCatalog>()
    return toCatalog(response)
  }

  /** Makes a role, directly above the default role. */
  async function createRole(organizationId: string, draft: RoleDraft): Promise<Role> {
    const response = await client
      .post(`tenancy/organizations/${organizationId}/roles`, { json: draft })
      .json<{ role: WireRole }>()
    return toRole(response.role)
  }

  async function updateRole(
    organizationId: string,
    roleId: string,
    update: RoleUpdate,
  ): Promise<Role> {
    const response = await client
      .patch(`tenancy/organizations/${organizationId}/roles/${roleId}`, { json: update })
      .json<{ role: WireRole }>()
    return toRole(response.role)
  }

  /** Deletes a role, revoking it from everybody who holds it. */
  async function deleteRole(organizationId: string, roleId: string): Promise<void> {
    await client.delete(`tenancy/organizations/${organizationId}/roles/${roleId}`)
  }

  /**
   * Puts the roles in a new order: every role but the administrator and
   * default roles, highest first.
   */
  async function reorderRoles(organizationId: string, roleIds: string[]): Promise<Role[]> {
    const response = await client
      .put(`tenancy/organizations/${organizationId}/role-order`, {
        json: { role_ids: roleIds },
      })
      .json<{ roles: WireRole[] }>()
    return response.roles.map(toRole)
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
    createTeam,
    deleteTeam,
    renameTeam,
    changeTeamMemberRoles,
    changeOrganizationMemberRoles,
    listOrganizationRoles,
    listTeamRoles,
    createRole,
    updateRole,
    deleteRole,
    reorderRoles,
    removeTeamMember,
    leaveTeam,
    revokeTeamInvitation,
  } as const
}
