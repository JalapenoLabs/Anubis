// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody, Input, Select, SelectItem } from '@heroui/react'

// Misc
import { ROLE_OPTIONS } from '../../permissions'

type Props = {
  title: string
  /** The team invited into. Set exactly one of the two targets. */
  teamId?: string
  /** The organization invited into. Set exactly one of the two targets. */
  organizationId?: string
  /** Revalidates the roster, where the invitation now holds a place. */
  onInvited: () => void
}

/**
 * Invites an address to a team or to an organization.
 *
 * The invitation is the credential: the server mails a link, and the roster
 * shows the place it holds until somebody claims it. Only an admin sees this
 * card, so a failure here is a real one and is shown rather than swallowed.
 */
export function InviteMemberCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ email, setEmail ] = useState('')
  const [ role, setRole ] = useState('default')
  const [ isInviting, setIsInviting ] = useState(false)
  const [ outcome, setOutcome ] = useState<string | null>(null)

  async function onInvite(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setIsInviting(true)
    setOutcome(null)
    try {
      await api.inviteMember({
        email: email.trim(),
        teamId: props.teamId,
        organizationId: props.organizationId,
        roles: [ role ],
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
            type='email'
            label={t('common.email')}
            className='w-full'
            value={email}
            onChange={(event) => setEmail(event.currentTarget.value)}
          />
          <Select
            label={t('team.members.role')}
            className='w-48 shrink-0'
            selectedKeys={[ role ]}
            onSelectionChange={(keys) => {
              const [ first ] = Array.from(keys)
              if (first) {
                setRole(String(first))
              }
            }}
          >
            {
              ROLE_OPTIONS.map((option) => (
                <SelectItem key={option}>{
                    option
                  }</SelectItem>
              ))
            }
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
