// Copyright © 2026 Jalapeno Labs

import type { NotificationsPage } from '../api/types'

// Core
import { useCallback, useEffect } from 'react'
import useSWR from 'swr'

// Misc
import { useAnubisApi } from './AnubisProvider'
import { useCurrentUser } from './useCurrentUser'
import { useOptionalRealtime } from './RealtimeProvider'

const NOTIFICATIONS_KEY = 'anubis/notifications'

/**
 * How many notifications one fetch brings back.
 *
 * The bell shows a recent slice rather than a full inbox, and the badge counts
 * the whole of it regardless, so this is a display choice and not a limit on
 * what a person has.
 */
export const NOTIFICATIONS_PAGE_SIZE = 10

/** The topic the backend publishes a written notification on. */
const NOTIFICATIONS_TOPIC = 'notifications'

const EMPTY_PAGE: NotificationsPage = {
  notifications: [],
  unread: 0,
  page: 1,
  limit: NOTIFICATIONS_PAGE_SIZE,
  totalItems: 0,
  totalPages: 0,
}

/**
 * The signed-in user's inbox, kept current by the realtime bell channel.
 *
 * Nothing is fetched while signed out. Under a `RealtimeProvider` the hook
 * subscribes to `user:{id}:notifications` and refetches on every ping, which
 * is all the socket carries: the REST answer stays the only source of what a
 * notification says. Without a provider the inbox is still correct, it just
 * waits for the next fetch.
 */
export function useNotifications() {
  const api = useAnubisApi()
  const realtime = useOptionalRealtime()
  const { user } = useCurrentUser()

  const { data, error, isLoading, mutate } = useSWR<NotificationsPage>(
    user ? NOTIFICATIONS_KEY : null,
    () => api.listNotifications(1, NOTIFICATIONS_PAGE_SIZE),
  )

  useEffect(() => {
    if (!realtime || !user) {
      console.debug('useNotifications is not subscribed; it will refresh on its next fetch')
      return () => {}
    }

    return realtime.subscribe(
      `user:${user.id}:${NOTIFICATIONS_TOPIC}`,
      () => {
        void mutate()
      },
    )
  }, [ realtime, user, mutate ])

  const markRead = useCallback(async (notificationId: string) => {
    await api.markNotificationRead(notificationId)
    await mutate()
  }, [ api, mutate ])

  const markAllRead = useCallback(async () => {
    await api.markAllNotificationsRead()
    await mutate()
  }, [ api, mutate ])

  const page = data ?? EMPTY_PAGE

  return {
    notifications: page.notifications,
    unread: page.unread,
    isLoading,
    error,
    refresh: mutate,
    markRead,
    markAllRead,
  } as const
}

export type NotificationsResult = ReturnType<typeof useNotifications>
