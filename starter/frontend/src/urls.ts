// Copyright © 2026 Jalapeno Labs

// Urls
export const UrlTree = {
  root: '/',
  signIn: '/sign-in',
  signUp: '/sign-up',
  forgotPassword: '/forgot-password',
  resetPassword: '/reset-password',
  verifyEmail: '/verify-email',
  /** Where the emailed link confirming a new address lands. */
  confirmEmailChange: '/change-email',
  settings: '/settings',
  settingsProfile: '/settings/profile',
  settingsSecurity: '/settings/security',
  teamSettings: '/teams/:teamId/settings',
  /**
   * The team's Developers section.
   *
   * Not `/developers`: the backend reserves that prefix for the API itself, so
   * a client route under it would answer JSON on a cold load.
   */
  teamDevelopers: '/teams/:teamId/developers',
  /** The team's audit log: who did what, admin-gated on both ends. */
  teamAuditLog: '/teams/:teamId/audit-log',
  organizationSettings: '/organizations/:organizationId/settings',
  /**
   * The organization's billing screen, and where Stripe returns the browser.
   *
   * The backend builds this URL from `APP_URL` before a checkout begins, so the
   * path is a contract with `anubis::billing` rather than a free choice. Not
   * `/billing`: the backend reserves that prefix for the API itself.
   */
  organizationBilling: '/organizations/:organizationId/billing',
  /** The prepaid credit ledger: every top-up, recharge, spend, and refund. */
  organizationBillingActivity: '/organizations/:organizationId/billing/activity',
  /** The organization's roles, highest first: the role editor's list. */
  organizationRoles: '/organizations/:organizationId/roles',
  /** One role's own page, where it is renamed, recoloured, and given permissions. */
  organizationRole: '/organizations/:organizationId/roles/:roleId',
  /** Making a role, on a page of its own. */
  organizationRoleNew: '/organizations/:organizationId/roles/new',
  /** Which teams reach the records the organization owns directly. */
  organizationAccess: '/organizations/:organizationId/access',
  claimInvitation: '/claim-invitation',
  /**
   * Where a connecting program such as Claude Code sends the person to approve
   * it.
   *
   * The backend redirects here from `/oauth/authorize`, so the path is a
   * contract with `anubis::oauth_server::CONSENT_PATH` rather than a free
   * choice, the same way the billing screen's is. Not under `/oauth`: the
   * backend reserves that prefix for the protocol itself.
   */
  consent: '/consent',
  /**
   * Where a person types the code a device without a browser shows, such as a
   * server agent signing in.
   *
   * The backend hands this URL to the device as its verification URI and
   * returns the browser here once the person decided, so the path is a
   * contract with `anubis::oauth_server::DEVICE_PATH`, like the consent
   * screen's. Named for RFC 8628's verification URI rather than `device`,
   * which is the key `anubis scaffold model Device` writes here.
   */
  deviceVerification: '/device',
  creativeConcepts: '/creative-concepts',
  creativeConcept: '/creative-concepts/:creativeConceptId',
  /**
   * A nested model owns a show page but no list page.
   *
   * Its table lives on its parent's show page, and this route is what the
   * depth below it attaches its own table to.
   */
  tangibleThing: '/tangible-things/:tangibleThingId',
  granularDetail: '/granular-details/:granularDetailId',
  // 🐺 anubis:urls
} as const
export type UrlValue = typeof UrlTree[keyof typeof UrlTree]

// Settings
export const UNKNOWN_ROUTE_REDIRECT_TO: UrlValue = UrlTree.root
/** Where a freshly signed-in user lands when no destination was preserved. */
export const POST_SIGN_IN_REDIRECT_TO: UrlValue = UrlTree.root
/**
 * Query parameter carrying the guarded destination through the auth pages.
 *
 * The destination rides in the URL rather than in router state because the
 * flow that needs it most starts cold: an invitation link opened from an email
 * is a fresh page load, and router state does not survive that.
 */
export const DESTINATION_PARAM = 'next'
/**
 * Query parameter an OAuth sign-in that failed comes back on.
 *
 * The backend redirects to the sign-in page with a machine-readable code
 * rather than a message, because the browser is the wrong place to explain
 * another system's failure; the page maps the code to a translated string.
 */
export const AUTH_ERROR_PARAM = 'error'
/** Query parameter naming the authorization request the consent screen shows. */
export const CONSENT_REQUEST_PARAM = 'request'
/**
 * Query parameter a device's link to the code-entry page carries its code in,
 * `anubis::oauth_server::USER_CODE_PARAM` on the backend.
 */
export const DEVICE_USER_CODE_PARAM = 'user_code'
/**
 * Query parameter the backend returns the browser to the code-entry page
 * with once the person decided, `approved` or `denied`.
 */
export const DEVICE_RESULT_PARAM = 'result'

// ///////////////////////////// //
//         Link factories        //
// ///////////////////////////// //

/**
 * Keeps a destination only when it points back into this app.
 *
 * The value arrives from the URL, so it is attacker-controlled: unchecked,
 * `/sign-in?next=https://evil.example` turns sign-in into an open redirect.
 * Root-relative paths (with their query and hash) pass. Absolute URLs,
 * protocol-relative `//host` and its `/\host` cousin, and control characters
 * do not.
 */
export function sanitizeDestination(destination: string | null | undefined): string | null {
  if (!destination) {
    return null
  }

  // Browsers strip tabs and newlines before resolving a URL, so a destination
  // such as `/<tab>/evil.example` resolves as `//evil.example` and leaves the app.
  const hasControlCharacters = [ ...destination ].some((character) => {
    const characterCode = character.charCodeAt(0)
    return characterCode <= 0x1f || characterCode === 0x7f
  })

  const isInternalPath = destination.startsWith('/')
    && !destination.startsWith('//')
    && !destination.startsWith('/\\')
    && !hasControlCharacters

  if (!isInternalPath) {
    console.debug('sanitizeDestination rejected a destination outside the app', destination)
    return null
  }

  return destination
}

/** Builds an auth-page link that returns the user to `destination` once signed in. */
export function getUrlWithDestination(url: UrlValue, destination: string | null | undefined): string {
  const safeDestination = sanitizeDestination(destination)
  if (!safeDestination) {
    return url
  }

  return `${url}?${DESTINATION_PARAM}=${encodeURIComponent(safeDestination)}`
}

/**
 * Builds the link that starts an OAuth sign-in with `provider`.
 *
 * This is a backend URL, not a router route: the browser leaves the SPA for
 * the provider's consent screen and comes back through the callback, which
 * sets the session cookie and redirects to the preserved destination.
 * `anubis scaffold oauth <provider>` writes the button that calls this.
 */
export function getOauthStartUrl(provider: string, destination: string | null | undefined): string {
  const safeDestination = sanitizeDestination(destination)
  if (!safeDestination) {
    return `/auth/oauth/${provider}/start`
  }

  return `/auth/oauth/${provider}/start?${DESTINATION_PARAM}=${encodeURIComponent(safeDestination)}`
}

/** The consent screen for one authorization request. */
export function getConsentRequestUrl(requestId: string): string {
  return `${UrlTree.consent}?${CONSENT_REQUEST_PARAM}=${encodeURIComponent(requestId)}`
}

export function getTeamSettingsUrl(teamId: string): string {
  return UrlTree.teamSettings.replace(':teamId', teamId)
}

export function getTeamDevelopersUrl(teamId: string): string {
  return UrlTree.teamDevelopers.replace(':teamId', teamId)
}

export function getTeamAuditLogUrl(teamId: string): string {
  return UrlTree.teamAuditLog.replace(':teamId', teamId)
}

export function getOrganizationSettingsUrl(organizationId: string): string {
  return UrlTree.organizationSettings.replace(':organizationId', organizationId)
}

export function getOrganizationBillingUrl(organizationId: string): string {
  return UrlTree.organizationBilling.replace(':organizationId', organizationId)
}

export function getOrganizationBillingActivityUrl(organizationId: string): string {
  return UrlTree.organizationBillingActivity.replace(':organizationId', organizationId)
}

export function getOrganizationRolesUrl(organizationId: string): string {
  return UrlTree.organizationRoles.replace(':organizationId', organizationId)
}

export function getOrganizationAccessUrl(organizationId: string): string {
  return UrlTree.organizationAccess.replace(':organizationId', organizationId)
}

export function getOrganizationRoleNewUrl(organizationId: string): string {
  return UrlTree.organizationRoleNew.replace(':organizationId', organizationId)
}

export function getOrganizationRoleUrl(organizationId: string, roleId: string): string {
  return UrlTree.organizationRole
    .replace(':organizationId', organizationId)
    .replace(':roleId', roleId)
}

export function getCreativeConceptUrl(creativeConceptId: string): string {
  return UrlTree.creativeConcept.replace(':creativeConceptId', creativeConceptId)
}

export function getTangibleThingUrl(tangibleThingId: string): string {
  return UrlTree.tangibleThing.replace(':tangibleThingId', tangibleThingId)
}

export function getGranularDetailUrl(granularDetailId: string): string {
  return UrlTree.granularDetail.replace(':granularDetailId', granularDetailId)
}

// 🐺 anubis:url-factories
