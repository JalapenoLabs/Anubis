// Copyright © 2026 Jalapeno Labs

import type { ConnectedClient } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage, useConnectedClients } from '@jalapenolabs/anubis'

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
 * The programs this account connected, such as Claude Code, each revocable.
 *
 * Revoking ends the connection on the server before the list refreshes, so a
 * row only disappears once the program's tokens have stopped working.
 */
export function ConnectedClientsCard() {
  const { t } = useTranslation()
  const { connectedClients, isLoading, revoke } = useConnectedClients()
  const [ revokingId, setRevokingId ] = useState<string | null>(null)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  async function onRevoke(connection: ConnectedClient) {
    setRevokingId(connection.id)
    setErrorMessage(null)
    try {
      await revoke(connection.id)
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setRevokingId(null)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.security.connectedClients.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.connectedClients.subtitle')
        }</p>
      <Table
        removeWrapper
        aria-label={t('settings.security.connectedClients.title')}
      >
        <TableHeader>
          <TableColumn>{
              t('settings.security.connectedClients.app')
            }</TableColumn>
          <TableColumn>{
              t('settings.security.connectedClients.permissions')
            }</TableColumn>
          <TableColumn>{
              t('settings.security.connectedClients.lastUsed')
            }</TableColumn>
          <TableColumn>{
              t('common.actions')
            }</TableColumn>
        </TableHeader>
        <TableBody
          items={connectedClients}
          isLoading={isLoading}
          emptyContent={isLoading
            ? t('common.loading')
            : t('settings.security.connectedClients.empty')}
        >
          {
            (connection: ConnectedClient) => (
              <TableRow key={connection.id}>
                <TableCell>
                  <p className='font-semibold'>{
                      connection.client.name
                    }</p>
                  <p className='text-sm opacity-60'>{
                      connection.client.verifiedHost
                        ? t('settings.security.connectedClients.verifiedBy', {
                          host: connection.client.verifiedHost,
                        })
                        : t('settings.security.connectedClients.unverified')
                    }</p>
                  <p className='text-sm opacity-60'>{
                      t('settings.security.connectedClients.connected', {
                        date: new Date(connection.createdAt).toLocaleDateString(),
                      })
                    }</p>
                </TableCell>
                <TableCell>
                  <div className='flex flex-wrap gap-1'>{
                      connection.scopes.length
                        ? connection.scopes.map((scope) => (
                          <Chip key={scope.name} size='sm' variant='flat'>{
                              scope.name
                            }</Chip>
                        ))
                        : <span className='opacity-70'>{
                            t('settings.security.connectedClients.identityOnly')
                          }</span>
                    }</div>
                </TableCell>
                <TableCell>{
                    new Date(connection.lastUsedAt).toLocaleString()
                  }</TableCell>
                <TableCell>
                  <Button
                    size='sm'
                    variant='light'
                    color='danger'
                    isDisabled={Boolean(revokingId)}
                    isLoading={revokingId === connection.id}
                    onPress={() => onRevoke(connection)}
                  >
                    <span>{
                        t('settings.security.connectedClients.revoke')
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
