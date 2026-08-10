// Copyright © 2026 Jalapeno Labs

// Urls
export const UrlTree = {
  root: '/',
  signIn: '/sign-in',
  signUp: '/sign-up',
  forgotPassword: '/forgot-password',
  resetPassword: '/reset-password',
  verifyEmail: '/verify-email',
  members: '/members',
  claimInvitation: '/claim-invitation',
  creativeConcepts: '/creative-concepts',
  creativeConcept: '/creative-concepts/:creativeConceptId',
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

export function getCreativeConceptUrl(creativeConceptId: string): string {
  return UrlTree.creativeConcept.replace(':creativeConceptId', creativeConceptId)
}

// 🐺 anubis:url-factories
