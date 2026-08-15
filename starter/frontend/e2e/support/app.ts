// Copyright © 2026 Jalapeno Labs

import type { Page } from '@playwright/test'

// Core
import { expect } from '@playwright/test'
import { randomUUID } from 'node:crypto'

// Misc
import { UrlTree } from '../../src/urls'
// Node runs these specs as real ES modules, where a JSON import carries an
// attribute. The application's own imports go through Vite, which does not.
import enUS from '../../src/locales/en-US.json' with { type: 'json' }
import creativeConceptsEnUS from '../../src/locales/models/creativeConcepts.en-US.json' with { type: 'json' }
import tangibleThingsEnUS from '../../src/locales/models/tangibleThings.en-US.json' with { type: 'json' }

/**
 * Every string the application renders, merged the way `i18n.ts` merges them.
 *
 * Specs address the interface by its accessible role and its visible text, and
 * that text is translated, so the locale files are where the text has to come
 * from. Reading it here means a renamed button breaks the spec at the locale
 * key rather than silently failing to find a stale literal.
 */
export const strings = {
  ...enUS,
  ...creativeConceptsEnUS,
  ...tangibleThingsEnUS,
}

/** The password every account this suite creates signs in with. */
const PASSWORD = 'e2e-password-9d41'

export type Account = {
  email: string
  password: string
}

/**
 * An address no other run will use.
 *
 * The suite drives the development database rather than a database of its own,
 * so runs accumulate rather than reset, and the only thing keeping them out of
 * each other's way is that no two ever claim the same account. This mirrors the
 * `it-<uuid>@example.com` convention the Rust narratives use.
 */
export function uniqueEmail(purpose: string): string {
  return `e2e-${purpose}-${randomUUID()}@example.com`
}

/**
 * Registers a fresh account and leaves the browser on the dashboard.
 *
 * Registration signs the new user straight in, so this is also how a spec that
 * needs a session gets one.
 */
export async function signUp(page: Page, purpose: string): Promise<Account> {
  const account: Account = {
    email: uniqueEmail(purpose),
    password: PASSWORD,
  }

  await page.goto(UrlTree.signUp)
  await fillCredentials(page, account)
  await page.getByRole('button', { name: strings.auth.signUp.action }).click()
  await expectDashboard(page)

  return account
}

/** Signs `account` in from the sign-in page the browser is already on. */
export async function signIn(page: Page, account: Account): Promise<void> {
  await fillCredentials(page, account)
  // Exactly, because the page also offers "Sign in with a passkey".
  await page.getByRole('button', { name: strings.auth.signIn.action, exact: true }).click()
}

/**
 * Fills the email and password pair the auth pages share.
 *
 * Both names are matched exactly: "Email" is a prefix of the button offering an
 * emailed code, and an accessible name matches on substring by default.
 */
async function fillCredentials(page: Page, account: Account): Promise<void> {
  await page.getByLabel(strings.common.email, { exact: true }).fill(account.email)
  await page.getByLabel(strings.common.password, { exact: true }).fill(account.password)
}

/** Waits for the signed-in dashboard, which is where a new session lands. */
export async function expectDashboard(page: Page): Promise<void> {
  await expect(page.getByRole('heading', { name: strings.dashboard.welcomeTitle }))
    .toBeVisible()
}

/**
 * The form carrying `submitLabel`, of the several a page can render.
 *
 * A show page holds its own edit form and its children's create form, and every
 * one of them labels a field "Name". Forms have no accessible name to address
 * them by, so the submit button names the form: it is the one control on it
 * that says what the form is for.
 */
export function formWithSubmit(page: Page, submitLabel: string) {
  return page.locator('form').filter({
    has: page.getByRole('button', { name: submitLabel }),
  })
}
