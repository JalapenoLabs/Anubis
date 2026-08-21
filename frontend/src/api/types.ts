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
  /**
   * Version of the stored avatar; null when the account has none.
   *
   * It changes with the picture, which is what makes `avatarUrl` a new URL
   * after an upload and every view of the account update at once.
   */
  avatarVersion: string | null
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

/**
 * One OAuth provider this deployment can sign in with.
 *
 * The backend lists only providers whose credentials are set, so a button
 * built from this never fails with `oauth_unavailable` at click time.
 */
export type OauthProvider = {
  /** The key the start URL is built from, such as `google`. */
  key: string
  /** The provider's name as a user reads it on a button, such as `Google`. */
  displayName: string
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

/** How a plan behaves at one of its limits. */
export type BillingEnforcement = 'hard' | 'soft'

/** What a plan allows of one metered thing. An absent limit is unlimited. */
export type BillingPlanLimit = {
  count: number
  enforcement: BillingEnforcement
}

/** What one billing interval of a plan costs. */
export type BillingPlanPrice = {
  stripePriceId: string
  /** In the currency's smallest unit: 2900 is $29.00. */
  amount: number
  /** Lowercase ISO 4217, as Stripe writes it. */
  currency: string
  /** True when the price is charged per seat rather than per organization. */
  perSeat: boolean
}

/**
 * A plan as the API reports it, which is the plan in force.
 *
 * The pricing grid reads `plans.generated.ts` instead, because the catalog is
 * configuration compiled into the same build rather than something to fetch.
 * This is the one plan an organization is on right now.
 */
export type BillingPlan = {
  key: string
  name: string
  description: string | null
  highlighted: boolean
  /** Keyed by interval: `monthly`, `yearly`. */
  prices: Record<string, BillingPlanPrice>
  /** Keyed by the application's own vocabulary: `seats`, `projects`. */
  limits: Record<string, BillingPlanLimit>
}

/**
 * The organization's subscription, mirroring Stripe's own record.
 *
 * `status` is Stripe's vocabulary verbatim: `trialing`, `active`, `past_due`,
 * `canceled`, and the rest. `past_due` still grants access, because Stripe is
 * still retrying the card.
 */
export type BillingSubscription = {
  id: string
  organizationId: string
  planKey: string
  stripeSubscriptionId: string
  status: string
  /** `monthly` or `yearly`. */
  interval: string
  /** Seats bought, which is the line item's quantity. */
  quantity: number
  currentPeriodEnd: string | null
  cancelAtPeriodEnd: boolean
  createdAt: string
  updatedAt: string
}

/** What a billing screen renders: the plan, the subscription, and the seats. */
export type BillingOverview = {
  plan: BillingPlan
  /** Absent on the free plan, which is the absence of a subscription. */
  subscription: BillingSubscription | null
  /** False when this deployment has no Stripe key, so nothing can be bought. */
  billingEnabled: boolean
  /** People who can reach the organization, claimed and invited alike. */
  seatsUsed: number
}

export type BillingCheckoutRequest = {
  planKey: string
  interval: string
}

/**
 * One notice in the signed-in user's inbox.
 *
 * `kind` is the machine-readable type, `<subject>.<event>`, and `title` and
 * `body` are the English text the backend stored when it wrote the row. An
 * application with a translated inbox looks its own copy up by `kind` and
 * renders that instead.
 */
export type AppNotification = {
  id: string
  /** The team the notice is about; null when it is about the person. */
  teamId: string | null
  kind: string
  title: string
  body: string | null
  /** Where the entry navigates, as an application path; null when nowhere. */
  href: string | null
  /** When the recipient read it; null while it is unread. */
  readAt: string | null
  createdAt: string
}

/** One page of the inbox, and the count the bell's badge shows. */
export type NotificationsPage = {
  /** Unread first, then newest first. */
  notifications: AppNotification[]
  /** Unread across the whole inbox, not just this page. */
  unread: number
  page: number
  limit: number
  totalItems: number
  totalPages: number
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
  avatar_version: string | null
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
    // A payload from an older backend has no version at all, which reads the
    // same as an account with no picture: the bare avatar URL.
    avatarVersion: wire.avatar_version ?? null,
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
