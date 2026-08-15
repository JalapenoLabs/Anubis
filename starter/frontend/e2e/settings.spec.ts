// Copyright © 2026 Jalapeno Labs

// Core
import { expect, test } from '@playwright/test'
import { randomUUID } from 'node:crypto'

// Misc
import { formWithSubmit, signUp, strings } from './support/app'
import { UrlTree } from '../src/urls'

/**
 * A write that has to survive the round trip to the database.
 *
 * The reload is the point. A form that only updates its own state looks
 * identical to one that saved, until the page is loaded again, and this is the
 * cheapest place to tell the two apart.
 */
test('a renamed profile survives a reload', async ({ page }) => {
  await signUp(page, 'profile')

  await page.goto(UrlTree.settingsProfile)
  const profileForm = formWithSubmit(page, strings.common.save)

  const firstName = `Ada ${randomUUID().slice(0, 8)}`
  const firstNameField = profileForm.getByLabel(strings.settings.profile.firstName, { exact: true })
  await firstNameField.fill(firstName)
  await profileForm.getByRole('button', { name: strings.common.save, exact: true }).click()
  await expect(page.getByText(strings.settings.profile.saved)).toBeVisible()

  await page.reload()
  await expect(firstNameField).toHaveValue(firstName)
})
