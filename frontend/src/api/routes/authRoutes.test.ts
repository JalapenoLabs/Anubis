// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Utility
import ky from 'ky'

// Misc
import { createAuthRoutes } from './authRoutes'

const WIRE_USER = {
  id: '3f1a2b3c-0000-4000-8000-0000000000ff',
  email: 'ada@example.com',
  email_verified: false,
  first_name: null,
  last_name: null,
  time_zone: 'America/Chicago',
  locale: 'en-US',
  platform_roles: [],
  password_change_required: false,
  created_at: '2026-10-05T00:00:00Z',
  avatar_version: null,
}

/** The auth routes over a fetch that records the body it was sent. */
function recordingRoutes() {
  const sent: Record<string, unknown>[] = []
  const client = ky.create({
    prefix: 'http://localhost/',
    retry: 0,
    fetch: async (input) => {
      const request = input instanceof Request
        ? input
        : new Request(input)
      sent.push(await request.json())
      return new Response(JSON.stringify({ user: WIRE_USER }), {
        status: 201,
        headers: { 'content-type': 'application/json' },
      })
    },
  })

  return { routes: createAuthRoutes(client), sent }
}

describe('register', () => {
  it('should send the browser\'s own time zone when the caller names none', async () => {
    const { routes, sent } = recordingRoutes()

    await routes.register({ email: 'ada@example.com', password: 'correct horse battery staple' })

    expect(sent[0].time_zone).toBe(new Intl.DateTimeFormat().resolvedOptions().timeZone)
    // No language is guessed: only the application knows which ones it ships.
    expect(sent[0].locale).toBeUndefined()
  })

  it('should send the time zone and locale the caller chose', async () => {
    const { routes, sent } = recordingRoutes()

    await routes.register({
      email: 'ada@example.com',
      password: 'correct horse battery staple',
      timeZone: 'Europe/Warsaw',
      locale: 'pl',
    })

    expect(sent[0]).toEqual({
      email: 'ada@example.com',
      password: 'correct horse battery staple',
      time_zone: 'Europe/Warsaw',
      locale: 'pl',
    })
  })
})

describe('acceptInvitation', () => {
  it('should send the token, the password, and the browser\'s own time zone', async () => {
    const { routes, sent } = recordingRoutes()

    const user = await routes.acceptInvitation('a-token', { password: 'correct horse battery staple' })

    expect(sent[0]).toEqual({
      token: 'a-token',
      password: 'correct horse battery staple',
      time_zone: new Intl.DateTimeFormat().resolvedOptions().timeZone,
    })
    expect(user.email).toBe('ada@example.com')
    expect(user.passwordChangeRequired).toBe(false)
  })

  it('should send the time zone and locale the caller chose', async () => {
    const { routes, sent } = recordingRoutes()

    await routes.acceptInvitation('a-token', {
      password: 'correct horse battery staple',
      timeZone: 'Europe/Warsaw',
      locale: 'pl',
    })

    expect(sent[0].time_zone).toBe('Europe/Warsaw')
    expect(sent[0].locale).toBe('pl')
  })
})
