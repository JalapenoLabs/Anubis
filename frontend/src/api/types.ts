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
  /**
   * Role keys held at the platform tier, empty for everyone but an operator.
   *
   * The platform is the deployment itself and has no roster, so the grant
   * lives on the account. Pass these to the generated `can` helper to draw an
   * operator surface, exactly as a membership's roles draw a team's.
   */
  platformRoles: readonly string[]
  /**
   * Whether the account must choose a new password before anything else.
   *
   * True after an operator set a temporary password. Until the account
   * changes it, every framework and application route but `me`, changing the
   * password, and signing out answers `403` with the code
   * `PASSWORD_CHANGE_REQUIRED`, so a shell reads this to send the person
   * straight to the screen that changes it.
   */
  passwordChangeRequired: boolean
}

export type UserEnvelope = {
  user: User
}

/** What an operator's invitation link says before it is accepted. */
export type InvitationPreview = {
  /** The address the invitation was sent to, and the account's address. */
  email: string
  /** When the link stops working, as an ISO 8601 timestamp. */
  expiresAt: string
}

/** What accepting an invitation sends: the first password and preferences. */
export type InvitationAcceptance = {
  password: string
  /**
   * IANA time zone the account starts in. Defaults to the browser's own,
   * exactly as registering does.
   */
  timeZone?: string
  /** BCP 47 locale the account starts in; omitted leaves the default. */
  locale?: string
}

export type MessageEnvelope = {
  message: string
}

export type Credentials = {
  email: string
  password: string
}

/**
 * A sign-up: the credentials, and the preferences the account starts with.
 *
 * `timeZone` defaults to the browser's own zone, so an account is created
 * already speaking the person's local time. `locale` has no such default
 * because only the application knows which languages it ships: pass the one
 * it resolved, or leave it out and the account starts on the server default.
 */
export type Registration = Credentials & {
  timeZone?: string
  locale?: string
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

/** One permission a connected client may hold, as the consent screen explains it. */
export type OauthScope = {
  /** The wire name, such as `samples:read`. */
  name: string
  /** What granting it lets the client do, written for the person approving. */
  description: string
}

/**
 * A program asking to act for the signed-in person, such as Claude Code.
 *
 * `name` is the client's own word. `verifiedHost` is the host that published
 * the client's metadata document, which the client cannot fake, and is null
 * for a client that registered itself: a screen shows the difference.
 */
export type OauthClient = {
  name: string
  clientId: string
  kind: 'metadata_document' | 'dynamic'
  verifiedHost: string | null
  clientUri: string | null
}

/**
 * Where an approval goes, which decides what the consent screen warns about.
 *
 * A `redirect` request sends a code back to the program's address, which the
 * screen must name. A `device` request was started by a program that showed
 * the person a code to enter here, and the approval goes to that device
 * wherever it is: the screen says so, because a code somebody else sent is
 * how this flow is abused.
 */
export type AuthorizationDelivery =
  | {
    kind: 'redirect'
    /** The host the code is delivered to. */
    host: string
    /** True when the code goes to a program on this device, worth a warning. */
    isLoopback: boolean
  }
  | { kind: 'device' }

/** What the consent screen asks the person to approve. */
export type AuthorizationRequest = {
  id: string
  client: OauthClient
  delivery: AuthorizationDelivery
  scopes: OauthScope[]
  expiresAt: string
}

/** One client the person connected, as account settings list it. */
export type ConnectedClient = {
  /** The grant's id, which revoking takes. */
  id: string
  client: OauthClient
  scopes: OauthScope[]
  createdAt: string
  lastUsedAt: string
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

/** A role as a roster or a membership names it. */
export type RoleSummary = {
  id: string
  name: string
  /** `#rrggbb`. */
  color: string
  /** Rank: higher outranks lower; the default role is 0. */
  position: number
}

/**
 * What the signed-in person may do somewhere, as the server resolved it.
 *
 * The SPA draws a control only when this allows it; the server decides
 * regardless, so this hides what would be refused rather than securing it.
 */
export type TenantAccess = {
  /** The roles that apply there, highest first, the default role included. */
  roles: RoleSummary[]
  /** The permission keys those roles carry, `<Subject>.<action>`. */
  permissions: string[]
  /** The position of the highest role that applies. */
  rank: number
  /** Whether the administrator role applies. */
  isAdministrator: boolean
}

export type MembershipTeam = {
  id: string
  name: string
  /** What the person may do in the team. */
  access: TenantAccess
}

export type MembershipOrganization = {
  id: string
  name: string
  /** False for somebody who only belongs to teams inside the organization. */
  isMember: boolean
  /** What the person may do organization-wide; empty for a team-only member. */
  access: TenantAccess
  teams: MembershipTeam[]
}

export type MembershipsOverview = {
  organizations: MembershipOrganization[]
}

/** The roles one member was granted on a single record of the team. */
export type ResourceRoles = {
  resourceId: string
  roles: RoleSummary[]
}

export type TeamRosterMember = {
  membershipId: string
  /** The member's email, from the account or the pending invitation. */
  email: string | null
  /** The roles granted team-wide, highest first; the default role is not listed. */
  roles: RoleSummary[]
  /** Roles granted on single records of the team. */
  resourceRoles: ResourceRoles[]
  /** True for invited members who have not claimed their membership yet. */
  pending: boolean
  /** The invitation to revoke; set exactly for a pending member. */
  invitationId: string | null
}

/**
 * One kind of record organizations own directly, as the viewer stands toward it.
 *
 * `kind` is the model the records are, spelled as `roles.yml` spells it, which
 * is also the subject of every permission on them.
 */
export type ResourceKindStanding = {
  kind: string
  /**
   * Whether the viewer's organization standing reaches every record of the
   * kind, which is what lets them see which teams reach which records.
   */
  reaches: boolean
  /** Whether the viewer may change which teams reach the kind. */
  mayManage: boolean
}

/** What one team reaches of one kind of the organization's own records. */
export type ResourceGrant = {
  teamId: string
  kind: string
  /** Every record of the kind, now and later. */
  every: boolean
  /** The specific records, by name, when `every` is false. */
  resources: { value: string, label: string }[]
}

/**
 * The access screen's whole read: the kinds the application shares, the
 * organization's teams, and the grants of every kind the viewer reaches.
 */
export type ResourceGrantsOverview = {
  kinds: ResourceKindStanding[]
  /** Empty for a viewer who reaches no kind, since there is nothing to read. */
  teams: { id: string, name: string }[]
  grants: ResourceGrant[]
}

/**
 * The end state one change asks for, for one team and one kind: every record
 * of it, specific ones, or (both empty) none.
 */
export type ResourceGrantChange = {
  every: boolean
  resourceIds: string[]
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
  /** Held organization-wide, or offered by the pending invitation. */
  roles: RoleSummary[]
  pending: boolean
  /** The invitation to revoke; set exactly for a pending member. */
  invitationId: string | null
}

/** One of an organization's roles, everything resolved. */
export type Role = {
  id: string
  /** The `roles.yml` key it was seeded from, or null for one the organization made. */
  key: string | null
  name: string
  color: string
  position: number
  /** The permission keys it carries, sorted. */
  permissions: string[]
  /** Whether the organization wrote its own permissions rather than following `roles.yml`. */
  customized: boolean
  /** The role everybody holds. */
  isDefault: boolean
  /** The administrator role at the top. */
  isAdministrator: boolean
  /** The tiers it may be granted at; empty for the default role, which is everybody's. */
  grantableAt: GrantTier[]
}

/** A tier a role is granted at. */
export type GrantTier = 'organization' | 'sub_tenant' | 'team'

/** An organization's roles, the vocabulary they draw from, and the viewer's standing. */
export type RoleCatalog = {
  /** Highest first. */
  roles: Role[]
  /** Every permission key a role may carry. */
  permissions: string[]
  /**
   * The keys among them some role in `roles.yml` grants: the only ones a role
   * can be given, by anybody, the administrator included.
   */
  grantable: string[]
  /** The viewer's standing where the catalog was read, which decides what they may change. */
  viewer: TenantAccess
}

/** A role to make. */
export type RoleDraft = {
  name: string
  color: string
  permissions: string[]
}

/** What to change about a role; `permissions: null` returns it to following `roles.yml`. */
export type RoleUpdate = {
  name?: string
  color?: string
  permissions?: string[] | null
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
  /** Roles of the target's organization to grant on claim, beyond the default role. */
  roleIds?: string[]
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

/** One line item of a subscription: a Stripe price, and how many of it. */
export type BillingSubscriptionItem = {
  id: string
  stripeItemId: string
  stripePriceId: string
  /** Null for a metered price, which bills usage rather than a quantity. */
  quantity: number | null
}

/**
 * One of the organization's subscriptions, mirroring Stripe's own record.
 *
 * The plan subscription names its plan and no subject; a subject subscription
 * pays for one of the application's records, named by `subjectType` and
 * `subjectId`, and names no plan.
 *
 * `status` is Stripe's vocabulary verbatim: `trialing`, `active`, `past_due`,
 * `canceled`, and the rest. `past_due` still grants access, because Stripe is
 * still retrying the card.
 */
export type BillingSubscription = {
  id: string
  organizationId: string
  /** The plan a plan subscription buys; null on a subject subscription. */
  planKey: string | null
  /** The model of the record a subject subscription pays for. */
  subjectType: string | null
  /** The id of that record. */
  subjectId: string | null
  stripeSubscriptionId: string
  status: string
  /** `monthly` or `yearly`. */
  interval: string
  /** Seats bought, which is the plan's line item's quantity. */
  quantity: number
  currentPeriodEnd: string | null
  cancelAtPeriodEnd: boolean
  items: BillingSubscriptionItem[]
  createdAt: string
  updatedAt: string
}

/** What a billing screen renders: the plan, the subscriptions, and the seats. */
export type BillingOverview = {
  plan: BillingPlan
  /** The plan subscription; absent on the free plan, which is its absence. */
  subscription: BillingSubscription | null
  /**
   * Every live subscription: the plan subscription first, then subjects. Null
   * for a member who may not update the subscription, who reads the plan but
   * not what each record costs.
   */
  subscriptions: BillingSubscription[] | null
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
  platform_roles: readonly string[]
  password_change_required: boolean
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
    // Same reasoning: a backend from before the platform tier sends no roles,
    // and holding none is what every ordinary account holds anyway.
    platformRoles: wire.platform_roles ?? [],
    // A backend from before temporary passwords never requires a change.
    passwordChangeRequired: wire.password_change_required ?? false,
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
