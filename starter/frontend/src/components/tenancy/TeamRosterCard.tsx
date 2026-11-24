// Copyright © 2026 Jalapeno Labs

import type { RoleCatalog, TeamRosterMember } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { MEMBERSHIP_SUBJECT, allows, outranksMember, useAnubisApi } from '@jalapenolabs/anubis'

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
import { RoleChips } from '../roles/RoleChips'
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
  /** The organization's roles, with the viewer's standing in the team. */
  catalog: RoleCatalog | undefined
  /** The signed-in user's address, so their own row offers no removal. */
  currentUserEmail: string
  /** Revalidates the roster after a change lands. */
  onChanged: () => void
}

/**
 * The team roster: who is here, what they may do, and who is still invited.
 *
 * Every member reads it. A row offers what the viewer may do to that member:
 * changing roles takes `Tenancy::Membership.update`, removing somebody
 * `Tenancy::Membership.destroy`, and both only for a member ranking below the
 * viewer. That mirrors rather than replaces the server's answer, and every
 * edit and removal goes through a dialog that keeps a refusal on screen when
 * the current state is what says no.
 */
export function TeamRosterCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ editing, setEditing ] = useState<TeamRosterMember | null>(null)
  const [ confirming, setConfirming ] = useState<PendingConfirmation | null>(null)

  const viewer = props.catalog?.viewer
  const mayEditRoles = allows(viewer, 'update', MEMBERSHIP_SUBJECT)
  const mayRemove = allows(viewer, 'destroy', MEMBERSHIP_SUBJECT)

  async function onSaveRoles(roleIds: string[]) {
    if (!editing) {
      console.debug('roles saved with nobody being edited')
      return
    }

    await api.changeTeamMemberRoles(props.teamId, editing.membershipId, roleIds)
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
      return <RoleChips roles={member.roles} />
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

    const isSelf = member.email === props.currentUserEmail
    const reachable = Boolean(viewer && outranksMember(viewer, member.roles, isSelf))
    // Your own roles are edited like anyone's, and the server decides whether
    // the team can spare you; only removing yourself has its own way out,
    // which is the danger zone below.
    return <div className='level-left gap-2'>
      { mayEditRoles && reachable
        ? <Button
            size='sm'
            variant='flat'
            onPress={() => setEditing(member)}
          >
            <span>{
                t('team.members.editRolesAction')
              }</span>
          </Button>
        : null
      }
      { isSelf
        ? <span className='opacity-60'>{
            t('team.members.you')
          }</span>
        : null
      }
      { !isSelf && mayRemove && reachable
        ? <Button
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
        : null
      }
    </div>
  }

  const columns = [
    { key: 'member', label: t('team.members.email') },
    { key: 'roles', label: t('team.members.rolesHeader') },
    { key: 'status', label: t('team.members.status') },
  ]
  if (mayEditRoles || mayRemove) {
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
        key={editing?.membershipId ?? 'nobody'}
        email={editing ? editing.email ?? '' : null}
        heldRoleIds={editing?.roles.map((role) => role.id) ?? []}
        catalog={props.catalog}
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
