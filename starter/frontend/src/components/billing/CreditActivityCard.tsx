// Copyright © 2026 Jalapeno Labs

import type { WalletEntry } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { useAnubisApi } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Card,
  CardBody,
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
import { formatMicros } from '../../money'

/** Entries per page, matching the framework's own list default. */
const PAGE_LIMIT = 25

type Props = {
  organizationId: string
  /** The wallet's currency, which the entries are written in but do not carry. */
  currency: string
}

/**
 * The wallet's ledger, newest first: every top-up, recharge, spend, and refund.
 *
 * Amounts render to the micro-unit, because a sub-cent charge rounded to the
 * cent would leave the balance column impossible to add up from the amounts
 * beside it. Credits are green and signed, so the two directions read apart at
 * a glance.
 */
export function CreditActivityCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ page, setPage ] = useState(1)

  const entries = useSWR(
    [ 'wallet-entries', props.organizationId, page ],
    () => api.listWalletEntries(props.organizationId, page, PAGE_LIMIT),
  )

  const totalPages = entries.data?.totalPages ?? 0

  if (entries.error) {
    return <Card className='relaxed'>
      <CardBody>
        <p className='compact text-danger'>{
            t('billing.activity.loadFailed')
          }</p>
        <div>
          <Button
            variant='flat'
            isLoading={entries.isValidating}
            onPress={() => entries.mutate()}
          >
            <span>{
                t('common.tryAgain')
              }</span>
          </Button>
        </div>
      </CardBody>
    </Card>
  }

  return <Card className='relaxed'>
    <CardBody>
      {/* `removeWrapper` drops HeroUI's own scroll container along with its
          card chrome, so five columns need one of their own. */}
      <div className='overflow-x-auto'>
        <Table removeWrapper aria-label={t('billing.activity.title')}>
          <TableHeader>
            <TableColumn>{
                t('billing.activity.date')
              }</TableColumn>
            <TableColumn>{
                t('billing.activity.kind')
              }</TableColumn>
            <TableColumn>{
                t('billing.activity.description')
              }</TableColumn>
            <TableColumn align='end'>{
                t('billing.activity.amount')
              }</TableColumn>
            <TableColumn align='end'>{
                t('billing.activity.balance')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={entries.data?.entries ?? []}
            isLoading={entries.isLoading}
            emptyContent={t('billing.activity.empty')}
          >
            {
              (entry: WalletEntry) => {
                // A spend takes what the wallet holds rather than failing, so
                // one that drained it asked for more than it took.
                const takenMicros = -entry.amountMicros

                return <TableRow key={entry.id}>
                  <TableCell className='whitespace-nowrap'>
                    <Tooltip
                      content={new Date(entry.createdAt).toLocaleString(undefined, {
                        dateStyle: 'long',
                        timeStyle: 'medium',
                      })}
                    >
                      <span>{
                          new Date(entry.createdAt).toLocaleDateString(undefined, { dateStyle: 'long' })
                        }</span>
                    </Tooltip>
                  </TableCell>
                  <TableCell>
                    <Chip size='sm' variant='flat'>{
                        // A kind a later backend adds renders as itself
                        // rather than as a missing key.
                        t(`billing.wallet.kinds.${entry.kind}`, { defaultValue: entry.kind })
                      }</Chip>
                  </TableCell>
                  <TableCell>
                    <span className='block'>{
                        entry.description
                          ?? t(`billing.activity.descriptions.${entry.kind}`, { defaultValue: '' })
                      }</span>
                    { entry.requestedMicros !== null && entry.requestedMicros > takenMicros
                      ? <span className='block text-tiny opacity-70'>{
                          t('billing.activity.partlyCovered', {
                            taken: formatMicros(takenMicros, props.currency, { precise: true }),
                            requested: formatMicros(entry.requestedMicros, props.currency, { precise: true }),
                          })
                        }</span>
                      : null
                    }
                  </TableCell>
                  <TableCell
                    className={
                      entry.amountMicros > 0
                        ? 'whitespace-nowrap text-right text-success'
                        : 'whitespace-nowrap text-right'
                    }
                  >{
                      formatMicros(entry.amountMicros, props.currency, { precise: true, signed: true })
                    }</TableCell>
                  <TableCell className='whitespace-nowrap text-right'>{
                      formatMicros(entry.balanceAfterMicros, props.currency, { precise: true })
                    }</TableCell>
                </TableRow>
              }
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
    </CardBody>
  </Card>
}
