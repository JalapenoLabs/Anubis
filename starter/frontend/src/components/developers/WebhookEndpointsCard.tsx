// Copyright © 2026 Jalapeno Labs

import type { WebhookEndpoint } from '../../api/routes/webhookRoutes'

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Card,
  CardBody,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'
import { TextAreaField, TextField } from '@jalapenolabs/anubis'
import { WebhookDeliveriesModal } from './WebhookDeliveriesModal'
import { WebhookSecretModal } from './WebhookSecretModal'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import {
  createWebhookEndpoint,
  deleteWebhookEndpoint,
  listWebhookEndpoints,
  updateWebhookEndpoint,
} from '../../api/routes/webhookRoutes'
import { isEventType, parseEventTypes } from '../../webhooks'

const subscribeSchema = z.object({
  // Deliberately not a full URL parser: the server owns the rule (https, and
  // no credentials) and answers with the reason. This only stops the obvious.
  url: z.string().trim().regex(/^https?:\/\/\S+$/),
  description: z.string().trim(),
  eventTypes: z
    .string()
    .trim()
    .refine((value) => {
      const parsed = parseEventTypes(value)
      return parsed.length > 0 && parsed.every(isEventType)
    }),
})

type SubscribeFormValues = z.infer<typeof subscribeSchema>
const resolver = zodResolver(subscribeSchema)

type Props = {
  teamId: string
  /** Only a team admin may subscribe endpoints, exactly as the API decides. */
  canManage: boolean
}

/**
 * A team's outgoing webhook subscriptions: subscribe, pause, inspect, remove.
 *
 * Deliveries live one click away rather than on this card, because the list a
 * team reads day to day is "where do my events go", and the log is what they
 * open when one of them did not arrive.
 */
export function WebhookEndpointsCard(props: Props) {
  const { t } = useTranslation()

  const endpoints = useSWR(
    [ 'webhook-endpoints', props.teamId ],
    () => listWebhookEndpoints(props.teamId),
  )
  const [ freshSecret, setFreshSecret ] = useState<string | null>(null)
  const [ inspecting, setInspecting ] = useState<WebhookEndpoint | null>(null)
  const [ busyId, setBusyId ] = useState<string | null>(null)

  const form = useForm<SubscribeFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      url: '',
      description: '',
      eventTypes: '',
    },
  })

  const onSubscribe = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      const created = await createWebhookEndpoint(props.teamId, {
        url: data.url.trim(),
        description: data.description.trim() || undefined,
        event_types: parseEventTypes(data.eventTypes),
      })
      form.reset({ url: '', description: '', eventTypes: '' })
      setFreshSecret(created.secret)
      await endpoints.mutate()
    }
    catch (error) {
      console.debug('subscribing a webhook endpoint failed', error)
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
  })

  async function onSetActive(endpoint: WebhookEndpoint, active: boolean) {
    setBusyId(endpoint.id)
    form.clearErrors('root')
    try {
      await updateWebhookEndpoint(props.teamId, endpoint.id, { active })
      await endpoints.mutate()
    }
    catch (error) {
      console.debug('pausing or resuming a webhook endpoint failed', error)
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
    finally {
      setBusyId(null)
    }
  }

  async function onDelete(endpoint: WebhookEndpoint) {
    setBusyId(endpoint.id)
    form.clearErrors('root')
    try {
      await deleteWebhookEndpoint(props.teamId, endpoint.id)
      await endpoints.mutate()
    }
    catch (error) {
      console.debug('deleting a webhook endpoint failed', error)
      form.setError('root', {
        message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
      })
    }
    finally {
      setBusyId(null)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('developers.webhooks.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('developers.webhooks.subtitle')
        }</p>
      <div className='relaxed'>
        <Table
          removeWrapper
          aria-label={t('developers.webhooks.title')}
        >
          <TableHeader>
            <TableColumn>{
                t('developers.webhooks.url')
              }</TableColumn>
            <TableColumn>{
                t('developers.webhooks.events')
              }</TableColumn>
            <TableColumn>{
                t('developers.webhooks.active')
              }</TableColumn>
            <TableColumn>{
                t('common.actions')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={endpoints.data?.webhook_endpoints ?? []}
            isLoading={endpoints.isLoading}
            emptyContent={t('developers.webhooks.empty')}
          >
            {
              (endpoint: WebhookEndpoint) => (
                <TableRow key={endpoint.id}>
                  <TableCell>
                    <span className='block break-all'>{
                        endpoint.url
                      }</span>
                    { endpoint.description
                      ? <span className='block text-small opacity-70'>{
                          endpoint.description
                        }</span>
                      : null
                    }
                  </TableCell>
                  <TableCell className='opacity-80'>{
                      endpoint.event_types.join(', ')
                    }</TableCell>
                  <TableCell>
                    <Switch
                      size='sm'
                      isSelected={endpoint.active}
                      isDisabled={!props.canManage || busyId === endpoint.id}
                      aria-label={t('developers.webhooks.active')}
                      onValueChange={(active) => onSetActive(endpoint, active)}
                    />
                  </TableCell>
                  <TableCell>
                    <div className='level-left gap-2'>
                      {/* `flat` rather than `light`: a neutral row action with
                          no background and no color is indistinguishable from
                          the cell text beside it. */}
                      <Button
                        size='sm'
                        variant='flat'
                        onPress={() => setInspecting(endpoint)}
                      >
                        <span>{
                            t('developers.webhooks.viewDeliveries')
                          }</span>
                      </Button>
                      <Button
                        size='sm'
                        variant='light'
                        color='danger'
                        isDisabled={!props.canManage || Boolean(busyId)}
                        isLoading={busyId === endpoint.id}
                        onPress={() => onDelete(endpoint)}
                      >
                        <span>{
                            t('common.delete')
                          }</span>
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              )
            }
          </TableBody>
        </Table>
      </div>
      { props.canManage
        ? <form onSubmit={onSubscribe}>
            <div className='compact'>
              <TextField
                control={form.control}
                name='url'
                label={t('developers.webhooks.urlLabel')}
                help={t('developers.webhooks.urlHelp')}
                placeholder='https://example.com/webhooks/anubis'
                isRequired
                className='w-full'
              />
            </div>
            <div className='compact'>
              <TextField
                control={form.control}
                name='description'
                label={t('developers.webhooks.descriptionLabel')}
                className='w-full'
              />
            </div>
            <div className='compact'>
              <TextAreaField
                control={form.control}
                name='eventTypes'
                label={t('developers.webhooks.eventsLabel')}
                help={t('developers.webhooks.eventsHelp')}
                placeholder='project.created, project.updated'
                isRequired
                className='w-full'
              />
            </div>
            <div className='level-right'>
              <Button
                type='submit'
                color='primary'
                isDisabled={!form.formState.isValid || form.formState.isSubmitting}
                isLoading={form.formState.isSubmitting}
              >
                <span>{
                    t('developers.webhooks.subscribeAction')
                  }</span>
              </Button>
            </div>
          </form>
        : <p className='opacity-70'>{
            t('developers.webhooks.adminOnly')
          }</p>
      }
      { form.formState.errors.root
        ? <p className='mt-2 text-danger'>{
            form.formState.errors.root.message
          }</p>
        : null
      }
      <WebhookSecretModal
        secret={freshSecret}
        onClose={() => setFreshSecret(null)}
      />
      <WebhookDeliveriesModal
        teamId={props.teamId}
        endpoint={inspecting}
        onClose={() => setInspecting(null)}
      />
    </CardBody>
  </Card>
}
