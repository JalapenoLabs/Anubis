// Copyright © 2026 Jalapeno Labs

import type { WebhookDeliveryStatus } from './api/routes/webhookRoutes'

/**
 * The lifecycle actions a scaffolded model publishes.
 *
 * Mirrors `anubis::webhooks::LIFECYCLE_ACTIONS`. The server refuses anything
 * else, so the screen refuses it first and says why in the user's language.
 */
export const LIFECYCLE_ACTIONS = [ 'created', 'updated', 'destroyed' ] as const

/**
 * An event type is `<model>.<action>`: a snake-case model, a dot, an action.
 *
 * The model half is the application's own, so nothing here validates it
 * against a list. What this catches is the half the framework defines, so a
 * subscription to `project.create` is refused where it was typed rather than
 * sitting there receiving nothing forever.
 */
const EVENT_TYPE = new RegExp(`^[a-z0-9]+(_[a-z0-9]+)*\\.(${LIFECYCLE_ACTIONS.join('|')})$`)

/** Returns true when `candidate` is shaped like an event type. */
export function isEventType(candidate: string): boolean {
  return EVENT_TYPE.test(candidate)
}

/**
 * Splits a typed list of event types into the array the API takes.
 *
 * Accepts commas, whitespace, and newlines as separators, because a developer
 * pasting a list has no reason to care which one this field wanted. Duplicates
 * collapse and blanks disappear; order is what was typed.
 */
export function parseEventTypes(input: string): string[] {
  const parsed: string[] = []

  for (const candidate of input.split(/[\s,]+/)) {
    const trimmed = candidate.trim()
    if (trimmed && !parsed.includes(trimmed)) {
      parsed.push(trimmed)
    }
  }

  return parsed
}

/** The HeroUI chip color each delivery status renders in. */
export const deliveryStatusColor = {
  pending: 'default',
  delivered: 'success',
  failed: 'warning',
  dead: 'danger',
} as const satisfies Record<WebhookDeliveryStatus, string>
