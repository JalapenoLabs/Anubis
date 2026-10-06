// Copyright © 2026 Jalapeno Labs

// Core
import { afterEach, describe, expect, it, vi } from 'vitest'

// Misc
import { createAnubisApi } from './createAnubisApi'

const USER_ID = '3f1a2b3c-0000-4000-8000-0000000000ff'

describe('avatarUrl', () => {
  it('should carry the version so a new picture is a new URL', () => {
    const api = createAnubisApi()

    const first = api.avatarUrl({ id: USER_ID, avatarVersion: 'WFy7Qm1s3TkPq0aZ' })
    const second = api.avatarUrl({ id: USER_ID, avatarVersion: 'Zx9Wv8Ut7Sr6Qp5O' })

    expect(first).toBe(`/users/${USER_ID}/avatar?v=WFy7Qm1s3TkPq0aZ`)
    expect(second).not.toBe(first)
  })

  it('should serve no URL at all for an account with no picture', () => {
    const api = createAnubisApi()

    // A URL that is known to 404 would paint the broken-image glyph over the
    // initials the fallback draws, so an account with no picture has none.
    expect(api.avatarUrl({ id: USER_ID, avatarVersion: null })).toBeUndefined()
  })

  it('should honor the prefix the application mounted the framework under', () => {
    const api = createAnubisApi({ prefix: '/framework' })

    expect(api.avatarUrl({ id: USER_ID, avatarVersion: 'WFy7Qm1s3TkPq0aZ' }))
      .toBe(`/framework/users/${USER_ID}/avatar?v=WFy7Qm1s3TkPq0aZ`)
  })
})

describe('hooks', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('should run the application\'s hooks on the framework\'s own requests', async () => {
    const fetchMock = vi.fn<(request: Request) => Promise<Response>>(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    const afterResponse = vi.fn()

    const api = createAnubisApi({
      hooks: {
        beforeRequest: [
          ({ request }) => {
            request.headers.set('X-Reported-Location', 'Meridian, Idaho, US')
          },
        ],
        afterResponse: [ afterResponse ],
      },
    })
    await api.logout()

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const sent = fetchMock.mock.calls[0][0]
    expect(new URL(sent.url).pathname).toBe('/auth/logout')
    expect(sent.headers.get('X-Reported-Location')).toBe('Meridian, Idaho, US')
    expect(afterResponse).toHaveBeenCalledTimes(1)
  })

  it('should send nothing extra when no hooks are given', async () => {
    const fetchMock = vi.fn<(request: Request) => Promise<Response>>(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)

    await createAnubisApi().logout()

    const sent = fetchMock.mock.calls[0][0]
    expect(sent.headers.has('X-Reported-Location')).toBe(false)
  })
})
