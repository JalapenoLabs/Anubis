// Copyright © 2026 Jalapeno Labs

import type { AppNotification, NotificationsPage } from '../types'
import type { KyInstance } from 'ky'

type WireNotification = {
  id: string
  team_id: string | null
  kind: string
  title: string
  body: string | null
  href: string | null
  read_at: string | null
  created_at: string
}

type WirePage = {
  notifications: WireNotification[]
  pagination: {
    page: number
    limit: number
    total_items: number
    total_pages: number
  }
  unread: number
}

function toNotification(wire: WireNotification): AppNotification {
  return {
    id: wire.id,
    teamId: wire.team_id,
    kind: wire.kind,
    title: wire.title,
    body: wire.body,
    href: wire.href,
    readAt: wire.read_at,
    createdAt: wire.created_at,
  }
}

/**
 * The signed-in user's own notification inbox.
 *
 * Every route here is the caller's own: notifications are addressed to a
 * person rather than owned by a team, so nothing takes an id but the
 * notification's, and another user's id answers `404`.
 */
export function createNotificationRoutes(client: KyInstance) {
  async function listNotifications(page = 1, limit = 10): Promise<NotificationsPage> {
    const response = await client
      .get('account/notifications', {
        searchParams: {
          page,
          limit,
        },
      })
      .json<WirePage>()

    return {
      notifications: response.notifications.map(toNotification),
      unread: response.unread,
      page: response.pagination.page,
      limit: response.pagination.limit,
      totalItems: response.pagination.total_items,
      totalPages: response.pagination.total_pages,
    }
  }

  /** Marks one notification read, answering with it as it now stands. */
  async function markNotificationRead(notificationId: string): Promise<AppNotification> {
    const response = await client
      .post(`account/notifications/${notificationId}/read`)
      .json<{ notification: WireNotification }>()
    return toNotification(response.notification)
  }

  /** Marks everything unread read, answering with how many that was. */
  async function markAllNotificationsRead(): Promise<number> {
    const response = await client
      .post('account/notifications/read-all')
      .json<{ marked: number }>()
    return response.marked
  }

  return {
    listNotifications,
    markNotificationRead,
    markAllNotificationsRead,
  } as const
}
