// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantDraft, ResourceGrantsOverview } from '@jalapenolabs/anubis'

// Core
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Navigate, useParams } from 'react-router'
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
  const api = useAnubisApi()
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
    {/* An application that shares no kind of record has nothing to show
        here, and its settings show no way in either. */}
    { grants.overview && !grants.overview.kinds.length
      ? <Navigate to={getOrganizationSettingsUrl(organization.id)} replace />
      : null
    }
    { grants.overview?.kinds.length
      ? <EditAccess
          organizationId={organization.id}
          overview={grants.overview}
          onSaved={async () => {
            // Read back directly rather than through a revalidation, which
            // keeps the old grants and resolves quietly when the read fails:
            // the screen must know whether what it shows is the server's.
            const fresh = await api.listResourceGrants(organization.id)
            await grants.refresh(fresh, { revalidate: false })
          }}
        />
      : null
    }
    { grants.overview
      ? null
      : <Spinner />
    }
  </AppShell>
}

type EditAccessProps = {
  organizationId: string
  overview: ResourceGrantsOverview
  /** Reads the grants back after a save, and rejects when it cannot. */
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

  // The server's grants, and the viewer's edits on top of them. Only the
  // edits are state: whatever the server answers next, a team added or gone
  // included, the screen reads every row from `saved` and lays an edit over
  // it only where one was made, so a background refresh never loses an edit
  // and never meets a row it has no draft for.
  const saved = useMemo(() => draftsOf(props.overview), [ props.overview ])
  const [ edits, setEdits ] = useState<Record<string, ResourceGrantDraft>>({})

  const changed = Object.keys(saved).filter(
    (key) => edits[key] && !isSameGrant(edits[key], saved[key]),
  )
  const reached = props.overview.kinds.filter((kind) => kind.reaches)

  // Each team and kind is its own end state, sent one after another. A
  // refusal stops the run there, names the team, and leaves that edit and
  // every one after it in place to save. The edits that were written leave
  // the screen only once the grants are read back, so until then, and for
  // good when the read fails, the screen shows what was written rather than
  // the grants from before it.
  async function onSave() {
    setIsSaving(true)
    setIsSaved(false)
    setErrorMessage(null)
    const written: string[] = []
    let refusal: string | null = null
    for (const key of changed) {
      const [ teamId, kind ] = key.split('/')
      try {
        await api.replaceResourceGrants(props.organizationId, teamId, kind, changeOf(edits[key]))
        written.push(key)
      }
      catch (error) {
        const team = props.overview.teams.find((candidate) => candidate.id === teamId)
        refusal = t('access.saveFailed', {
          team: team?.name ?? teamId,
          message: getApiErrorMessage(error) ?? t('common.somethingWentWrong'),
        })
        break
      }
    }

    try {
      await props.onSaved()
      setEdits((current) => Object.fromEntries(
        Object.entries(current).filter(([ kept ]) => !written.includes(kept)),
      ))
      setErrorMessage(refusal)
      setIsSaved(!refusal)
    }
    catch (error) {
      console.debug('ResourceAccessPage could not read the grants back after saving', error)
      setErrorMessage(refusal ?? t('common.somethingWentWrong'))
    }
    setIsSaving(false)
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
                draft={edits[`${team.id}/${kind.kind}`] ?? saved[`${team.id}/${kind.kind}`]}
                onChange={(draft) => {
                  const key = `${team.id}/${kind.kind}`
                  setIsSaved(false)
                  // An edit taken back is no edit: keeping it would hold the
                  // old value over whatever the server reads next, and the
                  // next save would undo somebody else's change with it. The
                  // choice must match too, so "only these" with nothing
                  // picked yet stays on screen to pick from.
                  setEdits((current) => {
                    if (draft.mode === saved[key].mode && isSameGrant(draft, saved[key])) {
                      return Object.fromEntries(
                        Object.entries(current).filter(([ kept ]) => kept !== key),
                      )
                    }
                    return { ...current, [key]: draft }
                  })
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
