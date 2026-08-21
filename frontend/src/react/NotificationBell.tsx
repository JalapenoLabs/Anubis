// Copyright © 2026 Jalapeno Labs

import type { AppNotification } from '../api/types'

// Core
import { useState } from 'react'

// User interface
import { Badge, Button, Divider, Popover, PopoverContent, PopoverTrigger, Spinner } from '@heroui/react'

// Misc
import { useNotifications } from './useNotifications'

/**
 * The bell's own strings.
 *
 * The package never imports i18next, so the defaults are English and a
 * translated shell passes its own. They are one object rather than four props
 * because a shell that translates one of them translates all of them.
 */
export type NotificationBellLabels = {
  /** The trigger's accessible name, since it carries an icon and no text. */
  bell: string
  /** The popover's heading. */
  heading: string
  /** What the popover says when the inbox is empty. */
  empty: string
  /** The action that clears the badge. */
  markAllRead: string
}

type Props = {
  /** Overrides for any of the English defaults. */
  labels?: Partial<NotificationBellLabels>
  /**
   * Sends the browser to a notification's `href`.
   *
   * Pass the router's own navigation, so pressing an entry stays inside the
   * single-page application. Without it the bell falls back to a full page
   * load, which is correct but slower.
   */
  onNavigate?: (href: string) => void
  /** Extra classes for the trigger button. */
  className?: string
}

const DEFAULT_LABELS: NotificationBellLabels = {
  bell: 'Notifications',
  heading: 'Notifications',
  empty: 'Nothing here yet.',
  markAllRead: 'Mark all read',
}

/**
 * The navbar bell: an unread badge, and the recent notices behind it.
 *
 * Everything it needs comes from `useNotifications`, so mounting it is the
 * whole of the wiring:
 *
 * ```tsx
 * <NotificationBell onNavigate={(href) => navigate(href)} />
 * ```
 *
 * Pressing an entry marks it read and follows its `href`, which is what makes
 * the badge mean "waiting for you" rather than "happened recently".
 */
export function NotificationBell(props: Props) {
  const labels = {
    ...DEFAULT_LABELS,
    ...props.labels,
  }
  const { notifications, unread, isLoading, markRead, markAllRead } = useNotifications()
  const [ isOpen, setIsOpen ] = useState(false)

  async function onEntryPress(notification: AppNotification) {
    setIsOpen(false)

    if (!notification.readAt) {
      try {
        await markRead(notification.id)
      }
      catch (error) {
        // Navigating still helps the reader, so a failed mark is reported and
        // stepped over rather than swallowing the press.
        console.debug('Marking a notification read failed', error)
      }
    }

    if (!notification.href) {
      return
    }

    if (props.onNavigate) {
      props.onNavigate(notification.href)
      return
    }

    window.location.assign(notification.href)
  }

  async function onMarkAllPress() {
    try {
      await markAllRead()
    }
    catch (error) {
      console.debug('Marking every notification read failed', error)
    }
  }

  return <Popover isOpen={isOpen} onOpenChange={setIsOpen} placement='bottom-end'>
    <PopoverTrigger>
      <Button
        isIconOnly
        variant='light'
        aria-label={labels.bell}
        className={props.className}
      >
        <Badge
          content={unread}
          color='danger'
          size='sm'
          isInvisible={!unread}
          aria-label={labels.bell}
        >
          <BellIcon />
        </Badge>
      </Button>
    </PopoverTrigger>
    <PopoverContent className='w-80 p-0'>
      <div className='flex w-full items-center justify-between gap-4 px-4 py-3'>
        <span className='font-semibold'>{labels.heading}</span>
        <Button
          size='sm'
          variant='light'
          isDisabled={!unread}
          onPress={onMarkAllPress}
        >
          <span>{labels.markAllRead}</span>
        </Button>
      </div>
      <Divider />
      { isLoading
        ? <div className='flex w-full justify-center p-6'>
            <Spinner size='sm' />
          </div>
        : null
      }
      { !isLoading && !notifications.length
        ? <p className='w-full px-4 py-6 text-center opacity-70'>{labels.empty}</p>
        : null
      }
      <ul className='max-h-96 w-full overflow-y-auto'>
        {
          notifications.map((notification) => (
            <li key={notification.id}>
              <button
                type='button'
                className='flex w-full flex-col gap-1 px-4 py-3 text-left hover:bg-default-100'
                onClick={() => {
                  void onEntryPress(notification)
                }}
              >
                <span className='flex w-full items-center gap-2'>
                  { notification.readAt
                    ? null
                    : <span
                        aria-hidden
                        className='h-2 w-2 shrink-0 rounded-full bg-danger'
                      />
                  }
                  <span className='font-medium'>{notification.title}</span>
                </span>
                { notification.body
                  ? <span className='text-small opacity-70'>{notification.body}</span>
                  : null
                }
                <span className='text-tiny opacity-50'>{
                    new Date(notification.createdAt).toLocaleString()
                  }</span>
              </button>
            </li>
          ))
        }
      </ul>
    </PopoverContent>
  </Popover>
}

/**
 * The bell glyph, drawn inline.
 *
 * The package depends on no icon set, and one path costs less than one would.
 */
function BellIcon() {
  return <svg
    aria-hidden
    viewBox='0 0 24 24'
    fill='none'
    stroke='currentColor'
    strokeWidth={1.5}
    strokeLinecap='round'
    strokeLinejoin='round'
    className='h-5 w-5'
  >
    <path d='M15 17h5l-1.4-1.4A2 2 0 0 1 18 14.2V11a6 6 0 1 0-12 0v3.2a2 2 0 0 1-.6 1.4L4 17h5' />
    <path d='M9 17a3 3 0 0 0 6 0' />
  </svg>
}
