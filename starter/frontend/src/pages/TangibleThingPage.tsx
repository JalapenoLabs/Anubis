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
import { TangibleThingForm } from '../components/TangibleThingForm'
import { GranularDetailsSection } from '../components/GranularDetailsSection' // 🐺 anubis:template-only
// 🐺 anubis:child-imports

// Misc
import { getCreativeConcept } from '../api/routes/creativeConceptRoutes'
import {
  TANGIBLE_THING_MODEL,
  deleteTangibleThing,
  getTangibleThing,
} from '../api/routes/tangibleThingRoutes'
import { can } from '../roles.generated'
import { UrlTree, getCreativeConceptUrl } from '../urls'

/**
 * One tangible thing, and the records it owns.
 *
 * A nested model owns a show page even though it owns no list page, because
 * this page is where the next depth down attaches its own section. The chain
 * above the record is fetched rather than assumed: a nested row carries only
 * its parent's id, and both the breadcrumb trail and the team a scaffolded
 * association scopes its options to come from the chain's root.
 */
export function TangibleThingPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()
  const { tangibleThingId } = useParams()

  const tangibleThing = useSWR(
    tangibleThingId ? [ 'tangible-thing', tangibleThingId ] : null,
    () => getTangibleThing(tangibleThingId ?? ''),
  )
  const record = tangibleThing.data?.tangible_thing ?? null

  const creativeConceptId = record?.creative_concept_id ?? ''
  const creativeConcept = useSWR(
    creativeConceptId ? [ 'creative-concept', creativeConceptId ] : null,
    () => getCreativeConcept(creativeConceptId),
  )
  const parent = creativeConcept.data?.creative_concept ?? null

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (tangibleThing.error || !tangibleThingId) {
    return <AppShell user={user}>
      <p className='relaxed opacity-70'>{
          t('tangibleThings.notFound')
        }</p>
      <Link to={UrlTree.creativeConcepts} className='text-primary'>{
          t('creativeConcepts.backToList')
        }</Link>
    </AppShell>
  }

  // Read once: the form writes into this team, and a scaffolded child's
  // association options are scoped to it.
  const teamId = parent?.team_id ?? ''
  const heldRoles = current?.team.roles ?? []
  const mayUpdate = can(heldRoles, 'update', TANGIBLE_THING_MODEL)
  const mayDestroy = can(heldRoles, 'destroy', TANGIBLE_THING_MODEL)

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
      label: parent?.name ?? t('common.loading'),
      to: creativeConceptId
        ? getCreativeConceptUrl(creativeConceptId)
        : undefined,
    },
    {
      label: record?.name ?? t('common.loading'),
    },
  ]

  async function onDelete() {
    await deleteTangibleThing(tangibleThingId ?? '')
    await navigate(
      creativeConceptId
        ? getCreativeConceptUrl(creativeConceptId)
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
                  t('tangibleThings.deleteAction')
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
              t('tangibleThings.fields.description')
            }</dt>
          <dd>{
              record?.description ?? t('tangibleThings.noDescription')
            }</dd>
        </div>
        {/* 🐺 anubis:show-fields */}
      </dl>
    </div>
    { record && mayUpdate
      ? <Card className='relaxed p-2'>
          <CardBody>
            <TangibleThingForm
              creativeConceptId={creativeConceptId}
              teamId={teamId}
              editing={record}
              onDone={() => {
                void tangibleThing.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
    <GranularDetailsSection tangibleThingId={tangibleThingId} teamId={teamId} /> {/* 🐺 anubis:template-only */}
    {/* 🐺 anubis:children */}
  </AppShell>
}
