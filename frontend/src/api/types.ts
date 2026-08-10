// Copyright © 2026 Jalapeno Labs

/**
 * The user shape returned by every auth endpoint.
 *
 * Mirrors the backend's UserResponse serializer. The M3 client generator will
 * replace these handwritten types with generated ones.
 */
export type User = {
  id: string
  email: string
  emailVerified: boolean
  createdAt: string
}

export type UserEnvelope = {
  user: User
}

export type MessageEnvelope = {
  message: string
}

export type Credentials = {
  email: string
  password: string
}

export type MembershipTeam = {
  id: string
  name: string
  roles: string[]
}

export type MembershipOrganization = {
  id: string
  name: string
  /** Organization-level roles; empty for users who only belong to teams. */
  roles: string[]
  teams: MembershipTeam[]
}

export type MembershipsOverview = {
  organizations: MembershipOrganization[]
}

export type TeamRosterMember = {
  membershipId: string
  /** The member's email, from the account or the pending invitation. */
  email: string | null
  roles: string[]
  /** True for invited members who have not claimed their membership yet. */
  pending: boolean
}

export type InviteMemberRequest = {
  email: string
  teamId?: string
  organizationId?: string
  roles?: string[]
}

export type ClaimedInvitation = {
  organization: {
    id: string
    name: string
  }
  team: {
    id: string
    name: string
  } | null
}

/** Raw wire shape; the backend serializes snake_case fields. */
export type WireUser = {
  id: string
  email: string
  email_verified: boolean
  created_at: string
}

export type WireUserEnvelope = {
  user: WireUser
}

export function toUser(wire: WireUser): User {
  return {
    id: wire.id,
    email: wire.email,
    emailVerified: wire.email_verified,
    createdAt: wire.created_at,
  }
}
