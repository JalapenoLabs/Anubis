// Copyright © 2026 Jalapeno Labs

import type { OrganizationRosterMember, RoleCatalog } from '@jalapenolabs/anubis'
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

type Props = {
  organizationId: string
  members: OrganizationRosterMember[]
  isLoading: boolean
  /** The organization's roles, with the viewer's organization-wide standing. */
  catalog: RoleCatalog | undefined
  /** The signed-in user's address, so their own row offers no removal. */
  currentUserEmail: string
  /** Revalidates the roster after a change lands. */
  onChanged: () => void
}

/**
 * The organization roster: its members, and the invitations still outstanding.
 *
 * A row offers what the viewer may do to that member: change their
 * organization-wide roles, remove them, or take back the invitation, each
 * only for somebody ranking below the viewer. Invitations into the
 * organization's teams belong to those teams' rosters, so each place appears
 * exactly once across the two screens.
 */
export function OrganizationRosterCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ confirming, setConfirming ] = useState<OrganizationRosterMember | null>(null)
  const [ editing, setEditing ] = useState<OrganizationRosterMember | null>(null)

  const viewer = props.catalog?.viewer
  const mayEditRoles = allows(viewer, 'update', MEMBERSHIP_SUBJECT)
  const mayRemove = allows(viewer, 'destroy', MEMBERSHIP_SUBJECT)

  async function onSaveRoles(roleIds: string[]) {
    if (!editing?.membershipId) {
      console.debug('organization roles saved with no claimed member being edited', editing)
      return
    }

    await api.changeOrganizationMemberRoles(props.organizationId, editing.membershipId, roleIds)
    props.onChanged()
  }

  async function onConfirmed() {
    if (!confirming) {
      console.debug('organization roster confirmation resolved with nothing pending')
      return
    }

    if (confirming.invitationId) {
      await api.revokeOrganizationInvitation(props.organizationId, confirming.invitationId)
    }
    else if (confirming.membershipId) {
      await api.removeOrganizationMember(props.organizationId, confirming.membershipId)
    }
    else {
      console.debug('an organization roster row has neither membership nor invitation', confirming)
      return
    }

    props.onChanged()
  }

  function renderCell(member: OrganizationRosterMember, columnKey: string): ReactNode {
    if (columnKey === 'member') {
      return member.email
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
    return <div className='level-left gap-2'>
      { mayEditRoles && reachable && !member.pending
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
            onPress={() => setConfirming(member)}
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
          t('organization.settings.rosterTitle')
        }</h3>
      <p className='compact opacity-70'>{
          t('organization.settings.rosterSubtitle')
        }</p>
      <Table
        removeWrapper
        aria-label={t('organization.settings.rosterTitle')}
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
            (member: OrganizationRosterMember) => (
              <TableRow key={member.membershipId ?? member.invitationId ?? member.email}>
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
        email={editing ? editing.email : null}
        heldRoleIds={editing?.roles.map((role) => role.id) ?? []}
        catalog={props.catalog}
        onSave={onSaveRoles}
        onClose={() => setEditing(null)}
      />
      <ConfirmActionModal
        isOpen={Boolean(confirming)}
        title={
          confirming?.pending
            ? t('team.members.revokeModalTitle')
            : t('team.members.removeModalTitle')
        }
        body={
          confirming?.pending
            ? t('team.members.revokeModalBody', { email: confirming?.email })
            : t('organization.settings.removeModalBody', { email: confirming?.email })
        }
        confirmLabel={
          confirming?.pending
            ? t('team.members.revokeAction')
            : t('team.members.removeAction')
        }
        onConfirm={onConfirmed}
        onClose={() => setConfirming(null)}
      />
    </CardBody>
  </Card>
}
