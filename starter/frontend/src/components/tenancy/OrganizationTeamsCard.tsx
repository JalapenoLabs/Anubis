// Copyright © 2026 Jalapeno Labs

import type { MembershipTeam } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody, Input } from '@heroui/react'
import { ConfirmActionModal } from './ConfirmActionModal'

// Misc
import { getTeamSettingsUrl } from '../../urls'

type Props = {
  organizationId: string
  /** The organization's teams, as the membership overview knows them. */
  teams: MembershipTeam[]
  /** True for an organization admin: teams can be created and dissolved. */
  canManage: boolean
  /** Revalidates the membership overview, which the switcher reads too. */
  onChanged: () => Promise<unknown>
}

/**
 * The organization's teams: open one, add one, dissolve one.
 *
 * Dissolving a team is an organization act rather than a team act, which is
 * why it lives here and not in the team's own settings: a team's admins run
 * the team, and the organization decides whether the team exists.
 */
export function OrganizationTeamsCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ name, setName ] = useState('')
  const [ isCreating, setIsCreating ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)
  const [ deleting, setDeleting ] = useState<MembershipTeam | null>(null)

  async function onCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setIsCreating(true)
    setErrorMessage(null)
    try {
      await api.createTeam(props.organizationId, name.trim())
      setName('')
      await props.onChanged()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsCreating(false)
    }
  }

  async function onDelete() {
    if (!deleting) {
      console.debug('team deletion confirmed with nothing pending')
      return
    }

    await api.deleteTeam(props.organizationId, deleting.id)
    await props.onChanged()
  }

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('organization.settings.teamsTitle')
        }</h3>
      <p className='compact opacity-70'>{
          t('organization.settings.teamsSubtitle')
        }</p>
      <ul className='compact'>
        {
          props.teams.map((team) => (
            <li key={team.id} className='level border-b border-default-200 py-2'>
              <Link
                to={getTeamSettingsUrl(team.id)}
                className='hover:underline'
              >{
                  team.name
                }</Link>
              { props.canManage
                ? <Button
                    size='sm'
                    variant='light'
                    color='danger'
                    onPress={() => setDeleting(team)}
                  >
                    <span>{
                        t('common.delete')
                      }</span>
                  </Button>
                : null
              }
            </li>
          ))
        }
      </ul>
      { props.teams.length
        ? null
        : <p className='compact opacity-70'>{
            t('organization.settings.teamsEmpty')
          }</p>
      }
      { props.canManage
        ? <form onSubmit={onCreate}>
            <div className='level items-start'>
              <Input
                label={t('organization.settings.teamName')}
                className='w-full'
                value={name}
                onChange={(event) => setName(event.currentTarget.value)}
              />
              <Button
                type='submit'
                color='primary'
                className='shrink-0 self-center'
                isDisabled={!name.trim() || isCreating}
                isLoading={isCreating}
              >
                <span>{
                    t('organization.settings.createTeamAction')
                  }</span>
              </Button>
            </div>
          </form>
        : null
      }
      { errorMessage
        ? <p className='mt-4 text-danger'>{
            errorMessage
          }</p>
        : null
      }
      <ConfirmActionModal
        isOpen={Boolean(deleting)}
        title={t('team.settings.dangerModalTitle', { team: deleting?.name })}
        body={t('team.settings.dangerModalBody')}
        confirmLabel={t('team.settings.dangerAction')}
        confirmationPhrase={deleting?.name}
        confirmationLabel={t('tenancy.typeToConfirm', { name: deleting?.name })}
        onConfirm={onDelete}
        onClose={() => setDeleting(null)}
      />
    </CardBody>
  </Card>
}
