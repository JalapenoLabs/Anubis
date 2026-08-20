// Copyright © 2026 Jalapeno Labs

import type { TeamRosterMember } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi } from '@jalapenolabs/anubis'

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
import { ConfirmActionModal } from './ConfirmActionModal'
import { EditMemberRolesModal } from './EditMemberRolesModal'

/** What an admin is being asked to confirm, and about whom. */
type PendingConfirmation = {
  kind: 'remove' | 'revoke'
  member: TeamRosterMember
}

type Props = {
  teamId: string
  members: TeamRosterMember[]
  isLoading: boolean
  /** True for a team admin: the roster becomes editable. */
  canManage: boolean
  /** The signed-in user's address, so their own row offers no removal. */
  currentUserEmail: string
  /** Revalidates the roster after a change lands. */
  onChanged: () => void
}

/**
 * The team roster: who is here, what they may do, and who is still invited.
 *
 * Every member reads it; only an admin edits it, and the controls simply are
 * not rendered otherwise. That mirrors rather than replaces the server's
 * answer: the endpoints behind each control refuse a non-admin with `403`, and
 * every edit and removal goes through a dialog that keeps the refusal on screen
 * when the current state is what says no.
 */
export function TeamRosterCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ editing, setEditing ] = useState<TeamRosterMember | null>(null)
  const [ confirming, setConfirming ] = useState<PendingConfirmation | null>(null)

  async function onSaveRoles(roles: string[]) {
    if (!editing) {
      console.debug('roles saved with nobody being edited')
      return
    }

    await api.changeTeamMemberRoles(props.teamId, editing.membershipId, roles)
    props.onChanged()
  }

  async function onConfirmed() {
    if (!confirming) {
      console.debug('roster confirmation resolved with nothing pending')
      return
    }

    const { kind, member } = confirming
    if (kind === 'revoke') {
      if (!member.invitationId) {
        console.debug('revoke confirmed for a member with no invitation', member)
        return
      }
      await api.revokeTeamInvitation(props.teamId, member.invitationId)
    }
    else {
      await api.removeTeamMember(props.teamId, member.membershipId)
    }
    props.onChanged()
  }

  function renderCell(member: TeamRosterMember, columnKey: string): ReactNode {
    if (columnKey === 'member') {
      return member.email ?? '...'
    }

    if (columnKey === 'roles') {
      // Role keys are the application's own vocabulary, from roles.yml, and are
      // shown as declared rather than translated.
      return member.roles.join(', ')
    }

    if (columnKey === 'status') {
      return <Chip
        size='sm'
        variant='flat'
        color={member.pending ? 'warning' : 'success'}
      >{
          member.pending
            ? t('team.members.pendingInvitation')
            : t('team.members.active')
        }</Chip>
    }

    // An admin edits their own roles like anyone's, and the server decides
    // whether the team can spare them; only removing yourself has its own way
    // out, which is the danger zone below.
    return <div className='level-left gap-2'>
      {/* `flat` rather than `light`: a neutral row action with no background
          and no color is indistinguishable from the cell text beside it. */}
      <Button
        size='sm'
        variant='flat'
        onPress={() => setEditing(member)}
      >
        <span>{
            t('team.members.editRolesAction')
          }</span>
      </Button>
      { member.email === props.currentUserEmail
        ? <span className='opacity-60'>{
            t('team.members.you')
          }</span>
        : <Button
            size='sm'
            variant='light'
            color='danger'
            onPress={() => setConfirming({
              kind: member.pending ? 'revoke' : 'remove',
              member,
            })}
          >
            <span>{
                member.pending
                  ? t('team.members.revokeAction')
                  : t('team.members.removeAction')
              }</span>
          </Button>
      }
    </div>
  }

  const columns = [
    { key: 'member', label: t('team.members.email') },
    { key: 'roles', label: t('team.members.rolesHeader') },
    { key: 'status', label: t('team.members.status') },
  ]
  if (props.canManage) {
    columns.push({ key: 'actions', label: t('common.actions') })
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('team.members.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('team.members.subtitle')
        }</p>
      <Table
        removeWrapper
        aria-label={t('team.members.title')}
      >
        <TableHeader columns={columns}>
          {
            (column) => (
              <TableColumn key={column.key}>{
                  column.label
                }</TableColumn>
            )
          }
        </TableHeader>
        <TableBody
          items={props.members}
          isLoading={props.isLoading}
          emptyContent={props.isLoading ? t('common.loading') : t('team.members.empty')}
        >
          {
            (member: TeamRosterMember) => (
              <TableRow key={member.membershipId}>
                {
                  (columnKey) => (
                    <TableCell>{
                        renderCell(member, String(columnKey))
                      }</TableCell>
                  )
                }
              </TableRow>
            )
          }
        </TableBody>
      </Table>
      <EditMemberRolesModal
        member={editing}
        onSave={onSaveRoles}
        onClose={() => setEditing(null)}
      />
      <ConfirmActionModal
        isOpen={Boolean(confirming)}
        title={
          confirming?.kind === 'revoke'
            ? t('team.members.revokeModalTitle')
            : t('team.members.removeModalTitle')
        }
        body={
          confirming?.kind === 'revoke'
            ? t('team.members.revokeModalBody', { email: confirming?.member.email })
            : t('team.members.removeModalBody', { email: confirming?.member.email })
        }
        confirmLabel={
          confirming?.kind === 'revoke'
            ? t('team.members.revokeAction')
            : t('team.members.removeAction')
        }
        onConfirm={onConfirmed}
        onClose={() => setConfirming(null)}
      />
    </CardBody>
  </Card>
}
