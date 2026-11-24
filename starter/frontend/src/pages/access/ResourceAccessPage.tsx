// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantDraft, ResourceGrantsOverview } from '@jalapenolabs/anubis'

// Core
import { useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import {
  changeOf,
  draftOf,
  getApiErrorMessage,
  isSameGrant,
  useAnubisApi,
  useCurrentUser,
  useResourceGrants,
} from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'
import { useRoleLabels } from '../../components/roles/useRoleLabels'

// UI
import { Button, Card, CardBody, Spinner, Tooltip } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { TeamKindGrant } from '../../components/access/TeamKindGrant'

// Misc
import { getOrganizationSettingsUrl } from '../../urls'

/**
 * Which teams reach the records the organization owns directly.
 *
 * Every member may open it; a kind the viewer's roles do not reach lists no
 * grants, and one they may not share is shown locked with the reason beneath
 * it. The server decides regardless.
 */
export function ResourceAccessPage() {
  const { t } = useTranslation()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '' } = useParams()

  const organization = memberships.organizations.find(
    (candidate) => candidate.id === organizationId,
  )
  const grants = useResourceGrants(organization?.isMember ? organizationId : null)

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (!organization?.isMember) {
    return <AppShell user={user}>
      { memberships.isLoading
        ? <Spinner />
        : <p className='opacity-70'>{
            t('organization.settings.notFound')
          }</p>
      }
    </AppShell>
  }

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('access.title') },
    ]}
  >
    { grants.overview
      ? <EditAccess
          organizationId={organization.id}
          overview={grants.overview}
          onSaved={grants.refresh}
        />
      : <Spinner />
    }
  </AppShell>
}

type EditAccessProps = {
  organizationId: string
  overview: ResourceGrantsOverview
  onSaved: () => Promise<unknown>
}

/**
 * Every team's grants of every kind the viewer reaches, as drafts keyed
 * `<team id>/<kind>`: neither half can hold a slash, so the key splits back.
 */
function draftsOf(overview: ResourceGrantsOverview): Record<string, ResourceGrantDraft> {
  const drafts: Record<string, ResourceGrantDraft> = {}
  for (const team of overview.teams) {
    for (const kind of overview.kinds) {
      const grant = overview.grants.find(
        (candidate) => candidate.teamId === team.id && candidate.kind === kind.kind,
      )
      drafts[`${team.id}/${kind.kind}`] = draftOf(grant)
    }
  }
  return drafts
}

/** The screen itself, mounted once the grants are known so it starts from them. */
function EditAccess(props: EditAccessProps) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const labels = useRoleLabels()
  const [ isSaving, setIsSaving ] = useState(false)
  const [ isSaved, setIsSaved ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const saved = useMemo(() => draftsOf(props.overview), [ props.overview ])
  const [ drafts, setDrafts ] = useState(saved)
  // A save, or somebody else's change, moves the grants underneath the
  // screen; the screen follows what the server has.
  useEffect(() => {
    setDrafts(saved)
  }, [ saved ])

  const changed = Object.keys(drafts).filter(
    (key) => !isSameGrant(drafts[key], saved[key]),
  )
  const reached = props.overview.kinds.filter((kind) => kind.reaches)

  // Each team and kind is its own end state, sent one after another; a
  // refusal stops there, says why, and the screen reads back what landed.
  async function onSave() {
    setIsSaving(true)
    setIsSaved(false)
    setErrorMessage(null)
    try {
      for (const key of changed) {
        const [ teamId, kind ] = key.split('/')
        await api.replaceResourceGrants(props.organizationId, teamId, kind, changeOf(drafts[key]))
      }
      setIsSaved(true)
    }
    catch (error) {
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      await props.onSaved()
      setIsSaving(false)
    }
  }

  const mayManageAny = reached.some((kind) => kind.mayManage)
  const saveDisabledReason = changed.length
    ? null
    : t('access.nothingChanged')

  return <>
    <div className='relaxed level'>
      <h2 className='title'>{
          t('access.title')
        }</h2>
      { mayManageAny
        ? <Tooltip content={saveDisabledReason} isDisabled={!saveDisabledReason}>
            <div>
              <Button
                color='primary'
                isDisabled={!changed.length || isSaving}
                isLoading={isSaving}
                onPress={onSave}
              >
                <span>{
                    t('common.save')
                  }</span>
              </Button>
            </div>
          </Tooltip>
        : null
      }
    </div>
    { errorMessage
      ? <p className='relaxed text-danger'>{
          errorMessage
        }</p>
      : null
    }
    { isSaved
      ? <p className='relaxed opacity-80'>{
          t('access.saved')
        }</p>
      : null
    }
    { props.overview.kinds.length
      ? null
      : <p className='relaxed opacity-70'>{
          t('access.nothingShared')
        }</p>
    }
    { props.overview.kinds
      .filter((kind) => !kind.reaches)
      .map((kind) => <Card key={kind.kind} className='relaxed p-2'>
        <CardBody>
          <p className='font-medium'>{
              labels.subjectLabel(kind.kind)
            }</p>
          <p className='text-small opacity-60'>{
              t('access.outOfReach')
            }</p>
        </CardBody>
      </Card>)
    }
    { reached.length
      ? props.overview.teams.map((team) => <Card key={team.id} className='relaxed p-2'>
          <CardBody>
            <h3 className='compact text-xl font-semibold'>{
                team.name
              }</h3>
            { reached.map((kind) => <TeamKindGrant
                key={kind.kind}
                organizationId={props.organizationId}
                teamName={team.name}
                kind={kind}
                kindLabel={labels.subjectLabel(kind.kind)}
                draft={drafts[`${team.id}/${kind.kind}`]}
                onChange={(draft) => {
                  setIsSaved(false)
                  setDrafts((current) => ({ ...current, [`${team.id}/${kind.kind}`]: draft }))
                }}
                isDisabled={isSaving}
              />)
            }
          </CardBody>
        </Card>)
      : null
    }
  </>
}
