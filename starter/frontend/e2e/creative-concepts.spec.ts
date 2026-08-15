// Copyright © 2026 Jalapeno Labs

// Core
import { expect, test } from '@playwright/test'
import { randomUUID } from 'node:crypto'

// Misc
import { formWithSubmit, signUp, strings } from './support/app'

/**
 * The golden path: from nothing to a parent record with a child under it.
 *
 * One narrative, because that is what the feature is. Splitting it into a test
 * per step would make each step set the previous ones up again, which is slower
 * and proves less: what matters is that the chain holds.
 */
test('a new account creates a creative concept and adds a tangible thing to it', async ({ page }) => {
  await signUp(page, 'concepts')

  await page.getByRole('link', { name: strings.creativeConcepts.navLink }).click()
  await expect(page.getByRole('heading', { name: strings.creativeConcepts.title }))
    .toBeVisible()

  // Named uniquely so the row assertions below can never match another run's.
  const conceptName = `Concept ${randomUUID().slice(0, 8)}`
  const conceptForm = formWithSubmit(page, strings.creativeConcepts.createAction)
  await conceptForm
    .getByLabel(strings.creativeConcepts.fields.name, { exact: true })
    .fill(conceptName)
  await conceptForm
    .getByRole('button', { name: strings.creativeConcepts.createAction })
    .click()

  const conceptRow = page.getByRole('row').filter({ hasText: conceptName })
  await expect(conceptRow).toBeVisible()

  await conceptRow.getByRole('link', { name: strings.creativeConcepts.open }).click()
  await expect(page.getByRole('heading', { name: conceptName })).toBeVisible()

  const thingName = `Thing ${randomUUID().slice(0, 8)}`
  const thingForm = formWithSubmit(page, strings.tangibleThings.createAction)
  await thingForm
    .getByLabel(strings.tangibleThings.fields.name, { exact: true })
    .fill(thingName)
  await thingForm
    .getByRole('button', { name: strings.tangibleThings.createAction })
    .click()

  // The child's own table, addressed by the label it carries, so this can
  // never accidentally read the parent's.
  const thingsTable = page.getByRole('grid', { name: strings.tangibleThings.title })
  await expect(thingsTable.getByRole('row').filter({ hasText: thingName })).toBeVisible()
})
