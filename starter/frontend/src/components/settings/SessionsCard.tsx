// Copyright © 2026 Jalapeno Labs

import type { AuthSession } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Card,
  CardBody,
  Chip,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'

/**
 * The account's live sessions, one row each, revocable.
 *
 * Revoking the current session is allowed: it is how a user signs this browser
 * out from the list, so the page refreshes the session afterwards and the auth
 * guard takes them to sign-in.
 */
export function SessionsCard() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()

  const sessions = useSWR('auth/sessions', () => api.listSessions())
  const [ revokingId, setRevokingId ] = useState<string | null>(null)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  async function onRevoke(session: AuthSession) {
    setRevokingId(session.id)
    setErrorMessage(null)
    try {
      await api.revokeSession(session.id)
      await sessions.mutate()
      if (session.isCurrent) {
        await refresh()
      }
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setRevokingId(null)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.security.sessions.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.sessions.subtitle')
        }</p>
      <Table
        removeWrapper
        aria-label={t('settings.security.sessions.title')}
      >
        <TableHeader>
          <TableColumn>{
              t('settings.security.sessions.signedIn')
            }</TableColumn>
          <TableColumn>{
              t('settings.security.sessions.expires')
            }</TableColumn>
          <TableColumn>{
              t('common.actions')
            }</TableColumn>
        </TableHeader>
        <TableBody
          items={sessions.data ?? []}
          isLoading={sessions.isLoading}
          emptyContent={t('common.loading')}
        >
          {
            (session: AuthSession) => (
              <TableRow key={session.id}>
                <TableCell>
                  <div className='level-left'>
                    <span>{
                        new Date(session.createdAt).toLocaleString()
                      }</span>
                    { session.isCurrent
                      ? <Chip size='sm' variant='flat' color='success'>{
                          t('settings.security.sessions.current')
                        }</Chip>
                      : null
                    }
                  </div>
                </TableCell>
                <TableCell>{
                    new Date(session.expiresAt).toLocaleDateString()
                  }</TableCell>
                <TableCell>
                  <Button
                    size='sm'
                    variant='light'
                    color='danger'
                    isDisabled={Boolean(revokingId)}
                    isLoading={revokingId === session.id}
                    onPress={() => onRevoke(session)}
                  >
                    <span>{
                        t('settings.security.sessions.revoke')
                      }</span>
                  </Button>
                </TableCell>
              </TableRow>
            )
          }
        </TableBody>
      </Table>
      { errorMessage
        ? <p className='mt-2 text-danger'>{
            errorMessage
          }</p>
        : null
      }
    </CardBody>
  </Card>
}
