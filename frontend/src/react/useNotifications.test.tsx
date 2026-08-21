// Copyright © 2026 Jalapeno Labs

import type { AnubisApi } from '../api/createAnubisApi'
import type { NotificationsPage } from '../api/types'
import type { RealtimeClient } from '../realtime/RealtimeClient'
import type { RealtimeListener } from '../realtime/listeners'
import type { ReactNode } from 'react'

// Core
import { describe, expect, it, vi } from 'vitest'

// Utility
import { renderHook, waitFor } from '@testing-library/react'
import { SWRConfig } from 'swr'

// Misc
import { AnubisProvider } from './AnubisProvider'
import { RealtimeProvider } from './RealtimeProvider'
import { useNotifications } from './useNotifications'

const USER = {
  id: 'a-user',
  email: 'reader@example.com',
  emailVerified: true,
  firstName: null,
  lastName: null,
  timeZone: 'UTC',
  locale: 'en-US',
  createdAt: '2026-08-20T00:00:00Z',
  avatarVersion: null,
}

function page(unread: number): NotificationsPage {
  return {
    notifications: [{
      id: 'a-notification',
      teamId: null,
      kind: 'invitation.received',
      title: 'You were invited',
      body: null,
      href: null,
      readAt: null,
      createdAt: '2026-08-20T00:00:00Z',
    }],
    unread,
    page: 1,
    limit: 10,
    totalItems: 1,
    totalPages: 1,
  }
}

/**
 * A double for the parts of the client the hook calls.
 *
 * The cast is the seam a test double always needs: the hook takes the whole
 * client from its provider, and this is the slice of it under test.
 */
function fakeApi(overrides: Partial<AnubisApi>): AnubisApi {
  return {
    me: vi.fn().mockResolvedValue(USER),
    listNotifications: vi.fn().mockResolvedValue(page(1)),
    markNotificationRead: vi.fn().mockResolvedValue(undefined),
    markAllNotificationsRead: vi.fn().mockResolvedValue(1),
    ...overrides,
  } as unknown as AnubisApi
}

/** A realtime client that records what was subscribed and can publish to it. */
function fakeRealtime() {
  const listeners = new Map<string, RealtimeListener>()

  return {
    client: {
      subscribe(channel: string, listener: RealtimeListener) {
        listeners.set(channel, listener)
        return () => {
          listeners.delete(channel)
        }
      },
    },
    channels: () => [ ...listeners.keys() ],
    publish(channel: string) {
      listeners.get(channel)?.({ channel, event: 'created', payload: {}})
    },
  }
}

/** Renders the hook under fresh providers, so no SWR cache is shared. */
function renderNotifications(api: AnubisApi, realtime?: ReturnType<typeof fakeRealtime>) {
  function Harness(props: { children: ReactNode }) {
    return <SWRConfig value={{ provider: () => new Map() }}>
      <AnubisProvider api={api}>
        { realtime
          // The provider takes the real client; the double implements the one
          // method the hook reaches for.
          ? <RealtimeProvider client={realtime.client as unknown as RealtimeClient}>{
              props.children
            }</RealtimeProvider>
          : props.children
        }
      </AnubisProvider>
    </SWRConfig>
  }

  return renderHook(() => useNotifications(), { wrapper: Harness })
}

describe('useNotifications', () => {
  it('should answer an empty inbox until the first page arrives', async () => {
    const api = fakeApi({})

    const { result } = renderNotifications(api)

    expect(result.current.notifications).toEqual([])
    expect(result.current.unread).toBe(0)

    await waitFor(() => {
      expect(result.current.unread).toBe(1)
    })
    expect(result.current.notifications).toHaveLength(1)
    expect(api.listNotifications).toHaveBeenCalledWith(1, 10)
  })

  it('should refetch when the recipient is pinged, and only for their channel', async () => {
    const listNotifications = vi.fn()
      .mockResolvedValueOnce(page(1))
      .mockResolvedValue(page(2))
    const realtime = fakeRealtime()

    const { result } = renderNotifications(fakeApi({ listNotifications }), realtime)

    await waitFor(() => {
      expect(result.current.unread).toBe(1)
    })
    expect(realtime.channels()).toEqual([ `user:${USER.id}:notifications` ])

    realtime.publish(`user:${USER.id}:notifications`)

    await waitFor(() => {
      expect(result.current.unread).toBe(2)
    })
  })

  it('should fetch nothing while signed out', async () => {
    const listNotifications = vi.fn()
    const api = fakeApi({
      me: vi.fn().mockResolvedValue(null),
      listNotifications,
    })

    const { result } = renderNotifications(api)

    await waitFor(() => {
      expect(result.current.isLoading).toBe(false)
    })
    expect(listNotifications).not.toHaveBeenCalled()
  })

  it('should refetch after marking one read and after marking them all read', async () => {
    const listNotifications = vi.fn()
      .mockResolvedValueOnce(page(1))
      .mockResolvedValue(page(0))
    const markNotificationRead = vi.fn().mockResolvedValue(undefined)
    const markAllNotificationsRead = vi.fn().mockResolvedValue(1)

    const { result } = renderNotifications(fakeApi({
      listNotifications,
      markNotificationRead,
      markAllNotificationsRead,
    }))

    await waitFor(() => {
      expect(result.current.unread).toBe(1)
    })

    await result.current.markRead('a-notification')
    expect(markNotificationRead).toHaveBeenCalledWith('a-notification')
    await waitFor(() => {
      expect(result.current.unread).toBe(0)
    })

    await result.current.markAllRead()
    expect(markAllNotificationsRead).toHaveBeenCalled()
  })
})
