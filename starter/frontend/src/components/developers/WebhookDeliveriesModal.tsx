// Copyright © 2026 Jalapeno Labs

import type { WebhookDelivery, WebhookEndpoint } from '../../api/routes/webhookRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Chip,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
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
import { listWebhookDeliveries, redeliverWebhookDelivery } from '../../api/routes/webhookRoutes'
import { deliveryStatusColor } from '../../webhooks'

/** Deliveries per page, matching the framework's own list default. */
const PAGE_LIMIT = 25

type Props = {
  teamId: string
  /** The endpoint whose log to show, or null when the modal is closed. */
  endpoint: WebhookEndpoint | null
  onClose: () => void
}

/**
 * One endpoint's delivery log: what was sent, how it went, and a way to retry.
 *
 * This is the debugging surface. A team whose receiver was down reads the
 * status, the attempt count, the HTTP code, and the error here, fixes their
 * end, and presses redeliver; that queues a fresh attempt and leaves the
 * original attempt's history where they can still see it.
 */
export function WebhookDeliveriesModal(props: Props) {
  const { t } = useTranslation()
  const [ page, setPage ] = useState(1)
  const [ redeliveringId, setRedeliveringId ] = useState<string | null>(null)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const endpointId = props.endpoint?.id
  const deliveries = useSWR(
    endpointId ? [ 'webhook-deliveries', props.teamId, endpointId, page ] : null,
    () => listWebhookDeliveries(props.teamId, endpointId ?? '', { page, limit: PAGE_LIMIT }),
  )

  async function onRedeliver(deliveryId: string) {
    if (!endpointId) {
      console.debug('redeliver pressed with no endpoint selected')
      return
    }

    setRedeliveringId(deliveryId)
    setErrorMessage(null)
    try {
      await redeliverWebhookDelivery(props.teamId, endpointId, deliveryId)
      setPage(1)
      await deliveries.mutate()
    }
    catch (error) {
      console.debug('redelivering a webhook failed', error)
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setRedeliveringId(null)
    }
  }

  const totalPages = deliveries.data?.pagination.total_pages ?? 0

  return <Modal
    size='5xl'
    isOpen={Boolean(props.endpoint)}
    onClose={() => {
      setPage(1)
      setErrorMessage(null)
      props.onClose()
    }}
  >
    <ModalContent>
      <ModalHeader className='flex-col items-start gap-1'>
        <span>{
            t('developers.webhooks.deliveries.title')
          }</span>
        <span className='text-small font-normal opacity-70 break-all'>{
            props.endpoint?.url
          }</span>
      </ModalHeader>
      <ModalBody>
        {/* Seven columns outgrow a narrow window; the scroll belongs to the
            table rather than to the modal, so the footer and the close button
            stay where the eye expects them. */}
        <div className='overflow-x-auto'>
          <Table
            removeWrapper
            aria-label={t('developers.webhooks.deliveries.title')}
          >
            <TableHeader>
              <TableColumn>{
                  t('developers.webhooks.deliveries.event')
                }</TableColumn>
              <TableColumn>{
                  t('developers.webhooks.deliveries.status')
                }</TableColumn>
              <TableColumn>{
                  t('developers.webhooks.deliveries.attempts')
                }</TableColumn>
              <TableColumn>{
                  t('developers.webhooks.deliveries.response')
                }</TableColumn>
              <TableColumn>{
                  t('developers.webhooks.deliveries.lastError')
                }</TableColumn>
              <TableColumn>{
                  t('developers.webhooks.deliveries.sentAt')
                }</TableColumn>
              <TableColumn>{
                  t('common.actions')
                }</TableColumn>
            </TableHeader>
            <TableBody
              items={deliveries.data?.webhook_deliveries ?? []}
              isLoading={deliveries.isLoading}
              emptyContent={t('developers.webhooks.deliveries.empty')}
            >
              {
                (delivery: WebhookDelivery) => (
                  <TableRow key={delivery.id}>
                    <TableCell>{
                        delivery.event_type
                      }</TableCell>
                    <TableCell>
                      <Chip size='sm' variant='flat' color={deliveryStatusColor[delivery.status]}>{
                          t(`developers.webhooks.deliveries.statuses.${delivery.status}`)
                        }</Chip>
                    </TableCell>
                    <TableCell>{
                        delivery.attempts
                      }</TableCell>
                    <TableCell>{
                        delivery.response_status ?? '-'
                      }</TableCell>
                    <TableCell className='max-w-xs'>
                      <Tooltip content={delivery.last_error} isDisabled={!delivery.last_error}>
                        <span className='block truncate opacity-70'>{
                            delivery.last_error ?? '-'
                          }</span>
                      </Tooltip>
                    </TableCell>
                    <TableCell className='whitespace-nowrap'>{
                        new Date(delivery.created_at).toLocaleString(undefined, {
                          dateStyle: 'short',
                          timeStyle: 'short',
                        })
                      }</TableCell>
                    <TableCell>
                      <Button
                        size='sm'
                        variant='light'
                        isDisabled={Boolean(redeliveringId)}
                        isLoading={redeliveringId === delivery.id}
                        onPress={() => onRedeliver(delivery.id)}
                      >
                        <span>{
                            t('developers.webhooks.deliveries.redeliver')
                          }</span>
                      </Button>
                    </TableCell>
                  </TableRow>
                )
              }
            </TableBody>
          </Table>
        </div>
        { errorMessage
          ? <p className='text-danger'>{errorMessage}</p>
          : null
        }
      </ModalBody>
      <ModalFooter className='justify-between'>
        { totalPages > 1
          ? <PaginationControl
              size='sm'
              page={page}
              total={totalPages}
              onChange={setPage}
            />
          : <span />
        }
        <Button variant='flat' onPress={props.onClose}>
          <span>{
              t('developers.webhooks.deliveries.close')
            }</span>
        </Button>
      </ModalFooter>
    </ModalContent>
  </Modal>
}
