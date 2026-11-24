// Copyright © 2026 Jalapeno Labs

import type { RoleCatalog } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { assignability, getApiErrorMessage, useAnubisApi } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody, Input, Select, SelectItem } from '@heroui/react'

/** The select's value for "no role beyond the one everybody holds". */
const DEFAULT_ROLE_CHOICE = 'default'

type Props = {
  title: string
  /** The team invited into. Set exactly one of the two targets. */
  teamId?: string
  /** The organization invited into. Set exactly one of the two targets. */
  organizationId?: string
  /** The organization's roles, with the inviter's standing where they invite. */
  catalog: RoleCatalog | undefined
  /** Revalidates the roster, where the invitation now holds a place. */
  onInvited: () => void
}

/**
 * Invites an address to a team or to an organization, offering one role.
 *
 * The invitation is the credential: the server mails a link, and the roster
 * shows the place it holds until somebody claims it. Only roles the inviter
 * may hand out are offered, because the server refuses the rest; everybody
 * invited holds the default role whatever else they are offered.
 */
export function InviteMemberCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ email, setEmail ] = useState('')
  const [ role, setRole ] = useState(DEFAULT_ROLE_CHOICE)
  const [ isInviting, setIsInviting ] = useState(false)
  const [ outcome, setOutcome ] = useState<string | null>(null)

  const { catalog } = props
  const tier = props.teamId
    ? 'team'
    : 'organization'
  const offered = catalog
    ? catalog.roles.filter((candidate) => assignability(catalog.viewer, candidate, tier) === null)
    : []
  const defaultRole = catalog?.roles.find((candidate) => candidate.isDefault)

  async function onInvite(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setIsInviting(true)
    setOutcome(null)
    try {
      await api.inviteMember({
        email: email.trim(),
        teamId: props.teamId,
        organizationId: props.organizationId,
        roleIds: role === DEFAULT_ROLE_CHOICE
          ? []
          : [ role ],
      })
      setOutcome(t('team.members.inviteSent', { email: email.trim() }))
      setEmail('')
      props.onInvited()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setOutcome(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsInviting(false)
    }
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          props.title
        }</h3>
      <form onSubmit={onInvite}>
        <div className='level items-start'>
          <Input
            id='invite-email'
            type='email'
            autoComplete='email'
            label={t('common.email')}
            className='w-full'
            value={email}
            onChange={(event) => setEmail(event.currentTarget.value)}
          />
          <Select
            id='invite-role'
            label={t('team.members.role')}
            className='w-56 shrink-0'
            selectedKeys={[ role ]}
            onSelectionChange={(keys) => {
              const [ first ] = Array.from(keys)
              if (first) {
                setRole(String(first))
              }
            }}
          >
            {[
              <SelectItem key={DEFAULT_ROLE_CHOICE}>{
                  defaultRole?.name ?? t('roles.onlyDefault')
                }</SelectItem>,
              ...offered.map((option) => (
                <SelectItem key={option.id}>{
                    option.name
                  }</SelectItem>
              )),
            ]}
          </Select>
          <Button
            type='submit'
            color='primary'
            className='shrink-0 self-center'
            isDisabled={!email.trim() || isInviting}
            isLoading={isInviting}
          >
            <span>{
                t('team.members.inviteAction')
              }</span>
          </Button>
        </div>
        { outcome
          ? <p className='mt-4 opacity-80'>{
              outcome
            }</p>
          : null
        }
      </form>
    </CardBody>
  </Card>
}
