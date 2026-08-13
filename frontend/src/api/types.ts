// Copyright © 2026 Jalapeno Labs

import type { WireCreationOptions, WireRequestOptions } from '../webauthn/ceremony'

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
  firstName: string | null
  lastName: string | null
  /** IANA time zone name, such as `America/Denver`. */
  timeZone: string
  /** BCP 47 locale tag, such as `en-US`. */
  locale: string
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

/**
 * A profile edit. An absent field stays as it is; a blank name clears it.
 *
 * Time zone and locale are required on the account, so blanking either is a
 * validation error rather than a reset.
 */
export type ProfileUpdate = {
  firstName?: string
  lastName?: string
  timeZone?: string
  locale?: string
}

/**
 * What a sign-in attempt produced: a session, or a second-factor challenge.
 *
 * Password sign-in and email-code sign-in both answer either way, so both
 * return this and the page decides which step comes next.
 */
export type SignInResult =
  | { status: 'signed-in', user: User }
  | { status: 'mfa-required', mfaToken: string }

/** One of the user's live browser sessions. */
export type AuthSession = {
  id: string
  createdAt: string
  expiresAt: string
  /** True for the session making the request that listed it. */
  isCurrent: boolean
}

/** One registered passkey, as the settings screen lists it. */
export type Passkey = {
  id: string
  name: string
  createdAt: string
  lastUsedAt: string | null
}

export type MfaStatus = {
  /** True once an enrollment has been confirmed with a code. */
  totpEnabled: boolean
}

/** A started TOTP enrollment, waiting for the code that confirms it. */
export type TotpEnrollment = {
  /** Base32 secret, for typing into an authenticator app by hand. */
  secret: string
  /** The provisioning URI the QR code encodes. */
  otpauthUri: string
}

export type ChangePasswordRequest = {
  currentPassword: string
  newPassword: string
}

export type EmailChangeRequest = {
  newEmail: string
  password: string
}

/** A started passkey registration: the browser's options plus the ceremony's token. */
export type PasskeyRegistrationChallenge = {
  stateToken: string
  creationOptions: WireCreationOptions
}

/** A started passkey login: the browser's options plus the ceremony's token. */
export type PasskeyLoginChallenge = {
  stateToken: string
  requestOptions: WireRequestOptions
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
  /** The invitation to revoke; set exactly for a pending member. */
  invitationId: string | null
}

/**
 * One row of an organization roster.
 *
 * An organization invitation creates no membership until it is claimed, so
 * `membershipId` is null exactly when `pending` is true. Invitations into the
 * organization's teams belong to those teams' rosters, not to this one.
 */
export type OrganizationRosterMember = {
  membershipId: string | null
  /** The member's email, from the account or the pending invitation. */
  email: string
  roles: string[]
  pending: boolean
  /** The invitation to revoke; set exactly for a pending member. */
  invitationId: string | null
}

export type TenancyOrganization = {
  id: string
  name: string
}

export type TenancyTeam = {
  id: string
  organizationId: string
  name: string
}

/** A created organization and the default team it starts with. */
export type CreatedOrganization = {
  organization: TenancyOrganization
  team: TenancyTeam
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
  first_name: string | null
  last_name: string | null
  time_zone: string
  locale: string
  created_at: string
}

export type WireUserEnvelope = {
  user: WireUser
}

/** What login and email-code verification answer when TOTP is confirmed. */
export type WireMfaChallenge = {
  mfa_required: boolean
  mfa_token: string
}

export function toUser(wire: WireUser): User {
  return {
    id: wire.id,
    email: wire.email,
    emailVerified: wire.email_verified,
    firstName: wire.first_name,
    lastName: wire.last_name,
    timeZone: wire.time_zone,
    locale: wire.locale,
    createdAt: wire.created_at,
  }
}

export function toSignInResult(wire: WireUserEnvelope | WireMfaChallenge): SignInResult {
  if ('mfa_token' in wire) {
    return {
      status: 'mfa-required',
      mfaToken: wire.mfa_token,
    }
  }

  return {
    status: 'signed-in',
    user: toUser(wire.user),
  }
}
