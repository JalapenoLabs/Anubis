// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import useSWR from 'swr'
import { useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { AppShell } from '../components/AppShell'
import { GranularDetailForm } from '../components/GranularDetailForm'

// Misc
import { getCreativeConcept } from '../api/routes/creativeConceptRoutes'
import { getTangibleThing } from '../api/routes/tangibleThingRoutes'
import {
  GRANULAR_DETAIL_MODEL,
  deleteGranularDetail,
  getGranularDetail,
} from '../api/routes/granularDetailRoutes'
import { can } from '../roles.generated'
import { UrlTree, getCreativeConceptUrl, getTangibleThingUrl } from '../urls'

/**
 * One granular detail, at the deepest ownership the scaffolder generates.
 *
 * The chain above the record is walked one link at a time, exactly as the
 * backend walks it: the record names its parent, the parent names the root,
 * and the root names the team. That trail is both the breadcrumb and the
 * source of the team a scaffolded association scopes its options to.
 */
export function GranularDetailPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()
  const { granularDetailId } = useParams()

  const granularDetail = useSWR(
    granularDetailId ? [ 'granular-detail', granularDetailId ] : null,
    () => getGranularDetail(granularDetailId ?? ''),
  )
  const record = granularDetail.data?.granular_detail ?? null

  const tangibleThingId = record?.tangible_thing_id ?? ''
  const tangibleThing = useSWR(
    tangibleThingId ? [ 'tangible-thing', tangibleThingId ] : null,
    () => getTangibleThing(tangibleThingId),
  )
  const parent = tangibleThing.data?.tangible_thing ?? null

  const creativeConceptId = parent?.creative_concept_id ?? ''
  const creativeConcept = useSWR(
    creativeConceptId ? [ 'creative-concept', creativeConceptId ] : null,
    () => getCreativeConcept(creativeConceptId),
  )
  const root = creativeConcept.data?.creative_concept ?? null

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (granularDetail.error || !granularDetailId) {
    return <AppShell user={user}>
      <p className='relaxed opacity-70'>{
          t('granularDetails.notFound')
        }</p>
      <Link to={UrlTree.creativeConcepts} className='text-primary'>{
          t('creativeConcepts.backToList')
        }</Link>
    </AppShell>
  }

  // Read once: the form writes into this team, and a scaffolded child's
  // association options are scoped to it.
  const teamId = root?.team_id ?? ''
  const heldRoles = current?.team.roles ?? []
  const mayUpdate = can(heldRoles, 'update', GRANULAR_DETAIL_MODEL)
  const mayDestroy = can(heldRoles, 'destroy', GRANULAR_DETAIL_MODEL)

  const breadcrumbs = [
    {
      label: t('app.title'),
      to: UrlTree.root,
    },
    {
      label: t('creativeConcepts.title'),
      to: UrlTree.creativeConcepts,
    },
    {
      label: root?.name ?? t('common.loading'),
      to: creativeConceptId
        ? getCreativeConceptUrl(creativeConceptId)
        : undefined,
    },
    {
      label: parent?.name ?? t('common.loading'),
      to: tangibleThingId
        ? getTangibleThingUrl(tangibleThingId)
        : undefined,
    },
    {
      label: record?.name ?? t('common.loading'),
    },
  ]

  async function onDelete() {
    await deleteGranularDetail(granularDetailId ?? '')
    await navigate(
      tangibleThingId
        ? getTangibleThingUrl(tangibleThingId)
        : UrlTree.creativeConcepts,
    )
  }

  return <AppShell user={user} breadcrumbs={breadcrumbs}>
    <div className='relaxed'>
      <div className='level'>
        <h2 className='title'>{
            record?.name ?? t('common.loading')
          }</h2>
        { mayDestroy
          ? <Button
              color='danger'
              variant='flat'
              onPress={onDelete}
            >
              <span>{
                  t('granularDetails.deleteAction')
                }</span>
            </Button>
          : null
        }
      </div>
      {/* A grid rather than a row, because the list grows: `scaffold field`
          adds an attribute above the anchor below, and a flex row would squash
          them all together and then overflow. Each attribute gets a cell of
          its own, wraps inside it, and folds to one column when the viewport
          is narrow. */}
      <dl className='mt-4 grid grid-cols-1 gap-x-8 gap-y-4 sm:grid-cols-2 lg:grid-cols-3'>
        <div>
          <dt className='text-sm opacity-60'>{
              t('granularDetails.fields.description')
            }</dt>
          <dd>{
              record?.description ?? t('granularDetails.noDescription')
            }</dd>
        </div>
        {/* 🐺 anubis:show-fields */}
      </dl>
    </div>
    { record && mayUpdate
      ? <Card className='relaxed p-2'>
          <CardBody>
            <GranularDetailForm
              tangibleThingId={tangibleThingId}
              teamId={teamId}
              editing={record}
              onDone={() => {
                void granularDetail.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
  </AppShell>
}
