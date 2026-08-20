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
import { CreativeConceptForm } from '../components/CreativeConceptForm'
import { TangibleThingsSection } from '../components/TangibleThingsSection' // 🐺 anubis:template-only
// 🐺 anubis:child-imports

// Misc
import {
  CREATIVE_CONCEPT_MODEL,
  deleteCreativeConcept,
  getCreativeConcept,
} from '../api/routes/creativeConceptRoutes'
import { can } from '../roles.generated'
import { UrlTree } from '../urls'

export function CreativeConceptPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()
  const { creativeConceptId } = useParams()

  const creativeConcept = useSWR(
    creativeConceptId ? [ 'creative-concept', creativeConceptId ] : null,
    () => getCreativeConcept(creativeConceptId ?? ''),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (creativeConcept.error || !creativeConceptId) {
    return <AppShell user={user}>
      <p className='relaxed opacity-70'>{
          t('creativeConcepts.notFound')
        }</p>
      <Link to={UrlTree.creativeConcepts} className='text-primary'>{
          t('creativeConcepts.backToList')
        }</Link>
    </AppShell>
  }

  const record = creativeConcept.data?.creative_concept ?? null
  // Read once: the form writes into this team, and a scaffolded child's
  // association options are scoped to it.
  const teamId = record?.team_id ?? ''
  const heldRoles = current?.team.roles ?? []
  const mayUpdate = can(heldRoles, 'update', CREATIVE_CONCEPT_MODEL)
  const mayDestroy = can(heldRoles, 'destroy', CREATIVE_CONCEPT_MODEL)

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
      label: record?.name ?? t('common.loading'),
    },
  ]

  async function onDelete() {
    await deleteCreativeConcept(creativeConceptId ?? '')
    await navigate(UrlTree.creativeConcepts)
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
                  t('creativeConcepts.deleteAction')
                }</span>
            </Button>
          : null
        }
      </div>
      <dl className='level-left mt-4 items-start gap-8'>
        <div>
          <dt className='text-sm opacity-60'>{
              t('creativeConcepts.fields.description')
            }</dt>
          <dd>{
              record?.description ?? t('creativeConcepts.noDescription')
            }</dd>
        </div>
        {/* 🐺 anubis:show-fields */}
      </dl>
    </div>
    { record && mayUpdate
      ? <Card className='relaxed p-2'>
          <CardBody>
            <CreativeConceptForm
              teamId={teamId}
              editing={record}
              onDone={() => {
                void creativeConcept.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
    <TangibleThingsSection creativeConceptId={creativeConceptId} teamId={teamId} /> {/* 🐺 anubis:template-only */}
    {/* 🐺 anubis:children */}
  </AppShell>
}
