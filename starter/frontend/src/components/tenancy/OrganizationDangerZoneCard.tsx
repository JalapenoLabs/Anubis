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
  organizationId: string
  organizationName: string
  /** True for an organization admin, who alone may dissolve it. */
  canDelete: boolean
  /** Revalidates the membership overview the switcher reads. */
  onChanged: () => Promise<unknown>
}

/**
 * Leaving the organization, and dissolving it.
 *
 * The last admin cannot leave and gets a `409` saying so: the way out of an
 * organization nobody else administers is to hand it over or to delete it,
 * never to leave it standing with no way back in.
 */
export function OrganizationDangerZoneCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const [ pending, setPending ] = useState<PendingAction | null>(null)

  async function onConfirmed() {
    if (pending === 'delete') {
      await api.deleteOrganization(props.organizationId)
    }
    else {
      await api.leaveOrganization(props.organizationId)
    }

    await props.onChanged()
    navigate(UrlTree.root)
  }

  return <Card className='relaxed border-2 border-danger p-2' shadow='none'>
    <CardBody>
      <h3 className='compact text-xl font-semibold text-danger'>{
          t('organization.settings.dangerTitle')
        }</h3>
      <p className='compact opacity-70'>{
          t('organization.settings.leaveSubtitle')
        }</p>
      <div className='level-right'>
        <Button
          color='danger'
          variant='flat'
          onPress={() => setPending('leave')}
        >
          <span>{
              t('organization.settings.leaveAction')
            }</span>
        </Button>
      </div>
      { props.canDelete
        ? <>
            <p className='compact mt-6 opacity-70'>{
                t('organization.settings.dangerSubtitle')
              }</p>
            <div className='level-right'>
              <Button
                color='danger'
                onPress={() => setPending('delete')}
              >
                <span>{
                    t('organization.settings.dangerAction')
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
            ? t('organization.settings.dangerModalTitle', { organization: props.organizationName })
            : t('organization.settings.leaveModalTitle', { organization: props.organizationName })
        }
        body={
          pending === 'delete'
            ? t('organization.settings.dangerModalBody')
            : t('organization.settings.leaveModalBody')
        }
        confirmLabel={
          pending === 'delete'
            ? t('organization.settings.dangerAction')
            : t('organization.settings.leaveAction')
        }
        confirmationPhrase={
          pending === 'delete'
            ? props.organizationName
            : undefined
        }
        confirmationLabel={t('tenancy.typeToConfirm', { name: props.organizationName })}
        onConfirm={onConfirmed}
        onClose={() => setPending(null)}
      />
    </CardBody>
  </Card>
}
