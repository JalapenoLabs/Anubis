// Copyright © 2026 Jalapeno Labs

import type { TeamRosterMember } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import {
  Button,
  Card,
  CardBody,
  Chip,
  Input,
  Select,
  SelectItem,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'
import { AppShell } from '../components/AppShell'

// Misc
import { RoleGrants } from '../roles.generated'

const roleOptions = Object.keys(RoleGrants)

export function MembersPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()

  const [ inviteEmail, setInviteEmail ] = useState('')
  const [ inviteRole, setInviteRole ] = useState('default')
  const [ isInviting, setIsInviting ] = useState(false)
  const [ inviteOutcome, setInviteOutcome ] = useState<string | null>(null)

  const teamId = current?.team.id ?? null
  const roster = useSWR(
    teamId ? `team-members/${teamId}` : null,
    () => api.listTeamMembers(teamId ?? ''),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  async function onInvite(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!teamId) {
      console.debug('invite submitted without a selected team')
      return
    }

    setIsInviting(true)
    setInviteOutcome(null)
    try {
      await api.inviteMember({
        email: inviteEmail.trim(),
        teamId,
        roles: [ inviteRole ],
      })
      setInviteOutcome(t('team.members.inviteSent', { email: inviteEmail.trim() }))
      setInviteEmail('')
      await roster.mutate()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setInviteOutcome(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsInviting(false)
    }
  }

  if (!current) {
    return <AppShell user={user}>
      <p className='opacity-70'>{
          t('team.members.noTeamSelected')
        }</p>
    </AppShell>
  }

  return <AppShell user={user}>
    <div className='relaxed'>
      <h2 className='title'>{
          t('team.members.title', { team: current.team.name })
        }</h2>
      <p className='opacity-70'>{
          t('team.members.subtitle')
        }</p>
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        <Table
          removeWrapper
          aria-label={t('team.members.title', { team: current.team.name })}
        >
          <TableHeader>
            <TableColumn>{
                t('team.members.email')
              }</TableColumn>
            <TableColumn>{
                t('team.members.rolesHeader')
              }</TableColumn>
            <TableColumn>{
                t('team.members.status')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={roster.data ?? []}
            isLoading={roster.isLoading}
            emptyContent={t('common.loading')}
          >
            {
              (member: TeamRosterMember) => (
                <TableRow key={member.membershipId}>
                  <TableCell>{
                      member.email ?? '...'
                    }</TableCell>
                  <TableCell>{
                      member.roles.join(', ')
                    }</TableCell>
                  <TableCell>
                    <Chip
                      size='sm'
                      variant='flat'
                      color={member.pending ? 'warning' : 'success'}
                    >{
                        member.pending
                          ? t('team.members.pendingInvitation')
                          : t('team.members.active')
                      }</Chip>
                  </TableCell>
                </TableRow>
              )
            }
          </TableBody>
        </Table>
      </CardBody>
    </Card>
    <Card className='relaxed p-2'>
      <CardBody>
        <h3 className='compact text-xl font-semibold'>{
            t('team.members.inviteTitle')
          }</h3>
        <form onSubmit={onInvite}>
          <div className='level items-start'>
            <Input
              type='email'
              label={t('common.email')}
              className='w-full'
              value={inviteEmail}
              onChange={(event) => setInviteEmail(event.currentTarget.value)}
            />
            <Select
              label={t('team.members.role')}
              className='w-48 shrink-0'
              selectedKeys={[ inviteRole ]}
              onSelectionChange={(keys) => {
                const [ first ] = Array.from(keys)
                if (first) {
                  setInviteRole(String(first))
                }
              }}
            >
              {
                roleOptions.map((role) => (
                  <SelectItem key={role}>{
                      role
                    }</SelectItem>
                ))
              }
            </Select>
            <Button
              type='submit'
              color='primary'
              className='shrink-0 self-center'
              isDisabled={!inviteEmail.trim() || isInviting}
              isLoading={isInviting}
            >
              <span>{
                  t('team.members.inviteAction')
                }</span>
            </Button>
          </div>
          { inviteOutcome
            ? <p className='mt-4 opacity-80'>{
                inviteOutcome
              }</p>
            : null
          }
        </form>
      </CardBody>
    </Card>
  </AppShell>
}
