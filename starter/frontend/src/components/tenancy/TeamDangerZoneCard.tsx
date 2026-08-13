// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { useAnubisApi } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { ConfirmActionModal } from './ConfirmActionModal'

// Misc
import { UrlTree } from '../../urls'

/** Which irreversible act is waiting on a confirmation. */
type PendingAction = 'leave' | 'delete'

type Props = {
  teamId: string
  teamName: string
  organizationId: string
  /** True for an admin of the team's organization, who alone may dissolve it. */
  canDeleteTeam: boolean
  /** Revalidates the membership overview the switcher reads. */
  onChanged: () => Promise<unknown>
}

/**
 * Leaving the team, and dissolving it: the two acts that cannot be undone.
 *
 * Leaving is refused with `409` when the caller is the team's last admin,
 * because a team with nobody to administer it is a team nobody can fix. The
 * modal keeps that message on screen instead of closing on it.
 */
export function TeamDangerZoneCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const [ pending, setPending ] = useState<PendingAction | null>(null)

  async function onConfirmed() {
    if (pending === 'delete') {
      await api.deleteTeam(props.organizationId, props.teamId)
    }
    else {
      await api.leaveTeam(props.teamId)
    }

    // The team is gone from under the current selection; the provider falls
    // back to another one as soon as the overview reloads.
    await props.onChanged()
    navigate(UrlTree.root)
  }

  return <Card className='relaxed border-2 border-danger p-2' shadow='none'>
    <CardBody>
      <h3 className='compact text-xl font-semibold text-danger'>{
          t('team.settings.dangerTitle')
        }</h3>
      <p className='compact opacity-70'>{
          t('team.settings.leaveSubtitle')
        }</p>
      <div className='level-right'>
        <Button
          color='danger'
          variant='flat'
          onPress={() => setPending('leave')}
        >
          <span>{
              t('team.settings.leaveAction')
            }</span>
        </Button>
      </div>
      { props.canDeleteTeam
        ? <>
            <p className='compact mt-6 opacity-70'>{
                t('team.settings.dangerSubtitle')
              }</p>
            <div className='level-right'>
              <Button
                color='danger'
                onPress={() => setPending('delete')}
              >
                <span>{
                    t('team.settings.dangerAction')
                  }</span>
              </Button>
            </div>
          </>
        : null
      }
      <ConfirmActionModal
        isOpen={Boolean(pending)}
        title={
          pending === 'delete'
            ? t('team.settings.dangerModalTitle', { team: props.teamName })
            : t('team.settings.leaveModalTitle', { team: props.teamName })
        }
        body={
          pending === 'delete'
            ? t('team.settings.dangerModalBody')
            : t('team.settings.leaveModalBody')
        }
        confirmLabel={
          pending === 'delete'
            ? t('team.settings.dangerAction')
            : t('team.settings.leaveAction')
        }
        confirmationPhrase={
          pending === 'delete'
            ? props.teamName
            : undefined
        }
        confirmationLabel={t('tenancy.typeToConfirm', { name: props.teamName })}
        onConfirm={onConfirmed}
        onClose={() => setPending(null)}
      />
    </CardBody>
  </Card>
}
