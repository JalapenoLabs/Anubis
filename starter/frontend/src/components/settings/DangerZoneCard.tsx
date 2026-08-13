// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { DeleteAccountModal } from './DeleteAccountModal'

type Props = {
  user: User
}

/** The one irreversible action on the page, kept apart from the rest. */
export function DangerZoneCard(props: Props) {
  const { t } = useTranslation()
  const [ isConfirming, setIsConfirming ] = useState(false)

  return <Card className='relaxed border-2 border-danger p-2' shadow='none'>
    <CardBody>
      <h3 className='compact text-xl font-semibold text-danger'>{
          t('settings.security.danger.title')
        }</h3>
      <p className='compact opacity-70'>{
          t('settings.security.danger.subtitle')
        }</p>
      <div className='level-right'>
        <Button
          color='danger'
          variant='flat'
          onPress={() => setIsConfirming(true)}
        >
          <span>{
              t('settings.security.danger.action')
            }</span>
        </Button>
      </div>
      <DeleteAccountModal
        user={props.user}
        isOpen={isConfirming}
        onClose={() => setIsConfirming(false)}
      />
    </CardBody>
  </Card>
}
