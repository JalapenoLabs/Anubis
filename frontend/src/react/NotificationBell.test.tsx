// Copyright © 2026 Jalapeno Labs

import type { AnubisApi } from '../api/createAnubisApi'
import type { AppNotification, NotificationsPage } from '../api/types'
import type { ReactNode } from 'react'

// Core
import { describe, expect, it, vi } from 'vitest'

// User interface
import { HeroUIProvider } from '@heroui/react'
import { NotificationBell } from './NotificationBell'

// Utility
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { SWRConfig } from 'swr'

// Misc
import { AnubisProvider } from './AnubisProvider'

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

const INVITATION: AppNotification = {
  id: 'a-notification',
  teamId: 'a-team',
  kind: 'invitation.claimed',
  title: 'Ada joined Acme',
  body: 'They hold: editor.',
  href: '/teams/a-team/settings',
  readAt: null,
  createdAt: '2026-08-20T00:00:00Z',
}

function page(notifications: AppNotification[], unread: number): NotificationsPage {
  return {
    notifications,
    unread,
    page: 1,
    limit: 10,
    totalItems: notifications.length,
    totalPages: notifications.length
      ? 1
      : 0,
  }
}

/** A double for the parts of the client the bell reaches through its hook. */
function fakeApi(overrides: Partial<AnubisApi>): AnubisApi {
  return {
    me: vi.fn().mockResolvedValue(USER),
    listNotifications: vi.fn().mockResolvedValue(page([ INVITATION ], 1)),
    markNotificationRead: vi.fn().mockResolvedValue(INVITATION),
    markAllNotificationsRead: vi.fn().mockResolvedValue(1),
    ...overrides,
  } as unknown as AnubisApi
}

function Harness(props: { api: AnubisApi, children: ReactNode }) {
  return <SWRConfig value={{ provider: () => new Map() }}>
    <AnubisProvider api={props.api}>
      <HeroUIProvider>{
          props.children
        }</HeroUIProvider>
    </AnubisProvider>
  </SWRConfig>
}

describe('NotificationBell', () => {
  it('should badge the unread count and list the inbox behind the bell', async () => {
    const api = fakeApi({})

    render(
      <Harness api={api}>
        <NotificationBell />
      </Harness>,
    )

    const bell = await screen.findByRole('button', { name: 'Notifications' })
    await waitFor(() => {
      expect(screen.getByText('1')).toBeTruthy()
    })

    await userEvent.click(bell)

    expect(await screen.findByText('Ada joined Acme')).toBeTruthy()
    expect(screen.getByText('They hold: editor.')).toBeTruthy()
  })

  it('should mark an entry read and navigate to its href when pressed', async () => {
    const markNotificationRead = vi.fn().mockResolvedValue({ ...INVITATION, readAt: 'now' })
    const onNavigate = vi.fn()

    render(
      <Harness api={fakeApi({ markNotificationRead })}>
        <NotificationBell onNavigate={onNavigate} />
      </Harness>,
    )

    await userEvent.click(await screen.findByRole('button', { name: 'Notifications' }))
    await userEvent.click(await screen.findByText('Ada joined Acme'))

    await waitFor(() => {
      expect(markNotificationRead).toHaveBeenCalledWith(INVITATION.id)
    })
    expect(onNavigate).toHaveBeenCalledWith('/teams/a-team/settings')
  })

  it('should offer mark-all-read only while something is unread', async () => {
    const markAllNotificationsRead = vi.fn().mockResolvedValue(1)

    render(
      <Harness api={fakeApi({
        listNotifications: vi.fn().mockResolvedValue(
          page([{ ...INVITATION, readAt: '2026-08-20T01:00:00Z' }], 0),
        ),
        markAllNotificationsRead,
      })}
      >
        <NotificationBell />
      </Harness>,
    )

    await userEvent.click(await screen.findByRole('button', { name: 'Notifications' }))

    const markAll = await screen.findByRole('button', { name: 'Mark all read' })
    expect(markAll.hasAttribute('disabled')).toBe(true)
    expect(markAllNotificationsRead).not.toHaveBeenCalled()
  })

  it('should say so when the inbox is empty, in the labels it was given', async () => {
    render(
      <Harness api={fakeApi({ listNotifications: vi.fn().mockResolvedValue(page([], 0)) })}>
        <NotificationBell labels={{ bell: 'Benachrichtigungen', empty: 'Nichts Neues.' }} />
      </Harness>,
    )

    await userEvent.click(await screen.findByRole('button', { name: 'Benachrichtigungen' }))

    expect(await screen.findByText('Nichts Neues.')).toBeTruthy()
  })
})
