// Copyright © 2026 Jalapeno Labs

import type { Role, TenantAccess } from '../api/types'
import type { DragEvent } from 'react'

// Core
import { useState } from 'react'

// User interface
import { Button, Chip, Tooltip } from '@heroui/react'

// Misc
import { isMovable, moveRole, reorderRequest } from './ladder'

/**
 * The ladder's own strings.
 *
 * The package never imports i18next, so the defaults are English and a
 * translated page passes its own, all of them at once.
 */
export type RoleLadderLabels = {
  /** The list's accessible name. */
  ladder: string
  /** The badge on the administrator role. */
  administrator: string
  /** The badge on the default role. */
  everybody: string
  /** The drag handle's accessible name. */
  drag: string
  moveUp: string
  moveDown: string
  /** Said on a role the viewer may not move. */
  pinned: string
}

type Props = {
  /** The whole ladder, highest first, as the catalog answers it. */
  roles: Role[]
  /** The viewer's organization-wide standing, which decides what moves. */
  viewer: TenantAccess
  /** Where each role's own page is. */
  hrefFor: (role: Role) => string
  /**
   * Sends the browser to a role's page.
   *
   * Pass the router's navigation so a press stays inside the application; a
   * plain link remains underneath, so a middle click still opens a new tab.
   */
  onNavigate?: (href: string) => void
  /**
   * Saves a new order: every role but the administrator and default ones,
   * highest first, exactly as the role-order endpoint takes it.
   */
  onReorder: (roleIds: string[]) => Promise<void>
  labels?: Partial<RoleLadderLabels>
}

const DEFAULT_LABELS: RoleLadderLabels = {
  ladder: 'Roles, highest first',
  administrator: 'Administrator',
  everybody: 'Everybody',
  drag: 'Drag to reorder',
  moveUp: 'Move up',
  moveDown: 'Move down',
  pinned: 'Only roles below your highest role can be moved',
}

/**
 * An organization's roles, highest first, reordered by dragging.
 *
 * Dragging is for a pointer; the move buttons beside each row do the same for
 * a keyboard, a screen reader, or a finger, so no way of using the page is
 * left out. A role the viewer may not move draws no handle and no buttons,
 * and a drop the server would refuse is never sent: the ladder only offers
 * what the role-order endpoint accepts. Each change is saved as it lands.
 */
export function RoleLadder(props: Props) {
  const labels = { ...DEFAULT_LABELS, ...props.labels }
  const [ dragging, setDragging ] = useState<string | null>(null)
  const [ pending, setPending ] = useState<Role[] | null>(null)

  // While a reorder is saving, the ladder shows where it is going rather
  // than snapping back and forth.
  const ladder = pending ?? props.roles

  async function move(roleId: string, toIndex: number) {
    const next = moveRole(ladder, props.viewer, roleId, toIndex)
    if (!next) {
      console.debug('RoleLadder ignored a move the server would refuse', { roleId, toIndex })
      return
    }

    setPending(next)
    try {
      await props.onReorder(reorderRequest(next))
    }
    finally {
      setPending(null)
    }
  }

  function onDrop(event: DragEvent<HTMLLIElement>, toIndex: number) {
    event.preventDefault()
    if (!dragging) {
      console.debug('RoleLadder received a drop with nothing being dragged')
      return
    }
    setDragging(null)
    void move(dragging, toIndex)
  }

  return <ol
    aria-label={labels.ladder}
    className='flex flex-col gap-1'
  >
    {
      ladder.map((role, index) => {
        const movable = isMovable(props.viewer, role) && !pending
        const href = props.hrefFor(role)
        const canMoveUp = movable && Boolean(moveRole(ladder, props.viewer, role.id, index - 1))
        const canMoveDown = movable && Boolean(moveRole(ladder, props.viewer, role.id, index + 1))

        return <li
          key={role.id}
          draggable={movable}
          onDragStart={(event) => {
            event.dataTransfer.effectAllowed = 'move'
            setDragging(role.id)
          }}
          onDragEnd={() => setDragging(null)}
          onDragOver={(event) => {
            if (dragging && moveRole(ladder, props.viewer, dragging, index)) {
              event.preventDefault()
            }
          }}
          onDrop={(event) => onDrop(event, index)}
          className={`flex items-center gap-3 rounded-medium border border-default-200 px-3 py-2 ${
            dragging === role.id ? 'opacity-50' : ''
          }`}
        >
          { movable
            ? <span
                aria-label={labels.drag}
                title={labels.drag}
                className='cursor-grab select-none opacity-60'
              >
                <svg width='12' height='16' viewBox='0 0 12 16' aria-hidden='true'>
                  <circle cx='3' cy='3' r='1.5' fill='currentColor' />
                  <circle cx='9' cy='3' r='1.5' fill='currentColor' />
                  <circle cx='3' cy='8' r='1.5' fill='currentColor' />
                  <circle cx='9' cy='8' r='1.5' fill='currentColor' />
                  <circle cx='3' cy='13' r='1.5' fill='currentColor' />
                  <circle cx='9' cy='13' r='1.5' fill='currentColor' />
                </svg>
              </span>
            : <Tooltip content={labels.pinned}>
                <span className='w-3 select-none text-center opacity-30'>·</span>
              </Tooltip>
          }
          <span
            aria-hidden='true'
            className='h-3 w-3 shrink-0 rounded-full'
            style={{ backgroundColor: role.color }}
          />
          <a
            href={href}
            className='min-w-0 flex-1 truncate font-medium hover:underline'
            onClick={(event) => {
              if (!props.onNavigate || event.metaKey || event.ctrlKey || event.shiftKey) {
                return
              }
              event.preventDefault()
              props.onNavigate(href)
            }}
          >{
              role.name
            }</a>
          { role.isAdministrator
            ? <Chip size='sm' variant='flat' color='danger'>{
                labels.administrator
              }</Chip>
            : null
          }
          { role.isDefault
            ? <Chip size='sm' variant='flat'>{
                labels.everybody
              }</Chip>
            : null
          }
          { movable
            ? <div className='flex gap-1'>
                <Tooltip content={labels.moveUp}>
                  <div>
                    <Button
                      isIconOnly
                      size='sm'
                      variant='light'
                      aria-label={labels.moveUp}
                      isDisabled={!canMoveUp}
                      onPress={() => move(role.id, index - 1)}
                    >
                      <svg width='12' height='12' viewBox='0 0 12 12' aria-hidden='true'>
                        <path d='M6 2 L11 8 H1 Z' fill='currentColor' />
                      </svg>
                    </Button>
                  </div>
                </Tooltip>
                <Tooltip content={labels.moveDown}>
                  <div>
                    <Button
                      isIconOnly
                      size='sm'
                      variant='light'
                      aria-label={labels.moveDown}
                      isDisabled={!canMoveDown}
                      onPress={() => move(role.id, index + 1)}
                    >
                      <svg width='12' height='12' viewBox='0 0 12 12' aria-hidden='true'>
                        <path d='M6 10 L11 4 H1 Z' fill='currentColor' />
                      </svg>
                    </Button>
                  </div>
                </Tooltip>
              </div>
            : null
          }
        </li>
      })
    }
  </ol>
}
