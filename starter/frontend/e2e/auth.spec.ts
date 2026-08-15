// Copyright © 2026 Jalapeno Labs

// Core
import { expect, test } from '@playwright/test'

// Misc
import { expectDashboard, signIn, signUp, strings } from './support/app'
import { DESTINATION_PARAM, UrlTree } from '../src/urls'

/**
 * The auth round trip, which is the one flow every other screen sits behind.
 *
 * The assertion that matters is the last one: the guard preserved where the
 * visitor was headed, so signing in lands there rather than on the dashboard.
 * That is the behaviour `sanitizeDestination` and the two route guards exist
 * for, and the only place it can be proven is a real browser with a real
 * session cookie.
 */
test('signing out sends a guarded page to sign-in, and signing in returns to it', async ({ page }) => {
  const account = await signUp(page, 'auth')

  await page.getByRole('button', { name: strings.common.accountMenu }).click()
  await page.getByRole('menuitem', { name: strings.common.signOut }).click()
  await expect(page.getByRole('heading', { name: strings.auth.signIn.title })).toBeVisible()

  // A cold load of a guarded URL, which is what an emailed link is.
  await page.goto(UrlTree.settingsProfile)
  await page.waitForURL((url) => (
    url.pathname === UrlTree.signIn
    && url.searchParams.get(DESTINATION_PARAM) === UrlTree.settingsProfile
  ))

  await signIn(page, account)
  await page.waitForURL((url) => url.pathname === UrlTree.settingsProfile)
  await expect(page.getByRole('heading', { name: strings.settings.profile.detailsTitle }))
    .toBeVisible()

  // And the session is real: the dashboard renders without another sign-in.
  await page.goto(UrlTree.root)
  await expectDashboard(page)
})
