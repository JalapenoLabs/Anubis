// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantDraft, ResourceKindStanding } from '@jalapenolabs/anubis'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useResourceOptions } from '@jalapenolabs/anubis'

// UI
import { ResourceGrantEditor } from '@jalapenolabs/anubis'

type Props = {
  organizationId: string
  teamName: string
  kind: ResourceKindStanding
  /** How the kind reads, already translated. */
  kindLabel: string
  draft: ResourceGrantDraft
  onChange: (draft: ResourceGrantDraft) => void
  isDisabled: boolean
}

/**
 * What one team reaches of one kind, with the picker's own search.
 *
 * Each row searches on its own, so typing in one team's picker never moves
 * another's; a viewer who may not share the kind fetches no options at all,
 * because the server would refuse them the list.
 */
export function TeamKindGrant(props: Props) {
  const { t } = useTranslation()
  const [ search, setSearch ] = useState('')
  const options = useResourceOptions(
    props.organizationId,
    props.kind.kind,
    search,
    props.kind.mayManage,
  )

  return <div className='compact'>
    <p className='mb-1 font-medium'>{
        props.kindLabel
      }</p>
    <ResourceGrantEditor
      draft={props.draft}
      onChange={props.onChange}
      options={options.options}
      onSearch={setSearch}
      isLoading={options.isLoading}
      isDisabled={props.isDisabled || !props.kind.mayManage}
      reason={props.kind.mayManage
        ? undefined
        : t('access.notManaged')}
      labels={{
        group: t('access.group', { team: props.teamName, kind: props.kindLabel }),
        nothing: t('access.nothing'),
        every: t('access.every'),
        only: t('access.only'),
        add: t('access.add'),
        remove: t('access.remove'),
      }}
    />
  </div>
}
