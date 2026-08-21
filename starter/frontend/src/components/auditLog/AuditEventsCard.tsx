// Copyright © 2026 Jalapeno Labs

import type { AuditChange, AuditEvent } from '../../api/routes/auditEventRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'

// UI
import {
  Card,
  CardBody,
  CardHeader,
  Chip,
  Pagination as PaginationControl,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
  Tooltip,
} from '@heroui/react'

// Misc
import { listTeamAuditEvents } from '../../api/routes/auditEventRoutes'
import { relativeTime } from '../../relativeTime'

/** Events per page, matching the framework's own list default. */
const PAGE_LIMIT = 25

type Props = {
  teamId: string
  /** Whether the caller holds the admin role; the API enforces the same rule. */
  canRead: boolean
}

/**
 * The team's audit log: who did what, newest first.
 *
 * Append-only on both ends. There is nothing to press here except a row, which
 * unfolds the change set the backend recorded, and the pager.
 *
 * `canRead` mirrors rather than replaces the server's answer: a member without
 * the admin role is refused by the endpoint too, and this only spares them a
 * table that would have come back `403`.
 */
export function AuditEventsCard(props: Props) {
  const { t } = useTranslation()
  const [ page, setPage ] = useState(1)

  const events = useSWR(
    props.canRead ? [ 'audit-events', props.teamId, page ] : null,
    () => listTeamAuditEvents(props.teamId, { page, limit: PAGE_LIMIT }),
  )

  const totalPages = events.data?.pagination.total_pages ?? 0

  return <Card className='relaxed'>
    <CardHeader className='flex-col items-start gap-1'>
      <h3 className='subtitle'>{
          t('auditLog.title')
        }</h3>
      <p className='text-small opacity-70'>{
          t('auditLog.subtitle')
        }</p>
    </CardHeader>
    <CardBody>
      { !props.canRead
        ? <p className='opacity-70'>{
            t('auditLog.adminOnly')
          }</p>
        : <>
            {/* `removeWrapper` drops HeroUI's own scroll container along with
                its card chrome, so five columns need one of their own. */}
            <div className='overflow-x-auto'>
              <Table removeWrapper aria-label={t('auditLog.title')}>
                <TableHeader>
                  <TableColumn>{
                      t('auditLog.actor')
                    }</TableColumn>
                  <TableColumn>{
                      t('auditLog.action')
                    }</TableColumn>
                  <TableColumn>{
                      t('auditLog.subject')
                    }</TableColumn>
                  <TableColumn>{
                      t('auditLog.when')
                    }</TableColumn>
                  <TableColumn>{
                      t('auditLog.changes')
                    }</TableColumn>
                </TableHeader>
                <TableBody
                  items={events.data?.audit_events ?? []}
                  isLoading={events.isLoading}
                  emptyContent={t('auditLog.empty')}
                >
                  {
                    (event: AuditEvent) => (
                      <TableRow key={event.id}>
                        <TableCell>{
                            // A null actor is the framework acting on nobody's
                            // behalf, which the log says out loud.
                            event.actor_name ?? t('auditLog.system')
                          }</TableCell>
                        <TableCell>
                          <Chip size='sm' variant='flat'>{
                              event.action
                            }</Chip>
                        </TableCell>
                        <TableCell>
                          <span className='block'>{
                              event.subject_label ?? event.subject_id ?? '-'
                            }</span>
                          <span className='block text-tiny opacity-70'>{
                              event.subject_type
                            }</span>
                        </TableCell>
                        <TableCell className='whitespace-nowrap'>
                          <Tooltip
                            content={new Date(event.created_at).toLocaleString(undefined, {
                              dateStyle: 'long',
                              timeStyle: 'medium',
                            })}
                          >
                            <span>{
                                relativeTime(event.created_at)
                              }</span>
                          </Tooltip>
                        </TableCell>
                        <TableCell>
                          <AuditChanges changes={event.changes} />
                        </TableCell>
                      </TableRow>
                    )
                  }
                </TableBody>
              </Table>
            </div>
            { totalPages > 1
              ? <div className='level-center mt-4'>
                  <PaginationControl
                    page={page}
                    total={totalPages}
                    onChange={setPage}
                    aria-label={t('common.pageOf', {
                      page,
                      totalPages,
                    })}
                  />
                </div>
              : null
            }
          </>
      }
    </CardBody>
  </Card>
}

type ChangesProps = {
  changes: Record<string, AuditChange>
}

/**
 * One row's change set: a count that unfolds into the fields that moved.
 *
 * The unfolded state is this component's own rather than the card's, because it
 * is nobody else's business and because HeroUI builds its rows from a cached
 * collection: a parent that re-rendered to expand one row would be asking the
 * table to rebuild every one of them.
 *
 * Values render as JSON because a change set spans every column of every model,
 * and the honest rendering of a value the screen knows nothing about is the
 * value.
 */
function AuditChanges(props: ChangesProps) {
  const { t } = useTranslation()
  const [ isExpanded, setIsExpanded ] = useState(false)
  const fields = Object.entries(props.changes ?? {})

  if (!fields.length) {
    return <span className='opacity-70'>{
        t('auditLog.noChanges')
      }</span>
  }

  return <div>
    <button
      type='button'
      className='text-small underline underline-offset-2 opacity-80 hover:opacity-100'
      aria-expanded={isExpanded}
      onClick={() => setIsExpanded(!isExpanded)}
    >{
        t('auditLog.changedFields', { count: fields.length })
      }</button>
    { isExpanded
      ? <dl className='mt-2 text-tiny'>
          { fields.map(([ field, change ]) => (
              <div key={field} className='mb-1'>
                <dt className='font-medium'>{field}</dt>
                <dd className='opacity-70'>
                  <span className='line-through'>{
                      JSON.stringify(change.old)
                    }</span>
                  {' → '}
                  <span>{
                      JSON.stringify(change.new)
                    }</span>
                </dd>
              </div>
          )) }
        </dl>
      : null
    }
  </div>
}
