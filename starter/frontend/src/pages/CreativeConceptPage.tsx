// Copyright © 2026 Jalapeno Labs

import type { TangibleThing } from '../api/routes/tangibleThingRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import useSWR from 'swr'
import { useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import {
  Button,
  Card,
  CardBody,
  Pagination,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'
import { AppShell } from '../components/AppShell'
import { TangibleThingForm } from '../components/TangibleThingForm'

// Misc
import {
  CREATIVE_CONCEPT_MODEL,
  deleteCreativeConcept,
  getCreativeConcept,
} from '../api/routes/creativeConceptRoutes'
import {
  TANGIBLE_THING_MODEL,
  deleteTangibleThing,
  listTangibleThings,
} from '../api/routes/tangibleThingRoutes'
import { can } from '../roles.generated'
import { UrlTree } from '../urls'

const PAGE_LIMIT = 10

export function CreativeConceptPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()
  const { creativeConceptId } = useParams()

  const [ page, setPage ] = useState(1)
  const [ editing, setEditing ] = useState<TangibleThing | null>(null)

  const concept = useSWR(
    creativeConceptId ? [ 'creative-concept', creativeConceptId ] : null,
    () => getCreativeConcept(creativeConceptId ?? ''),
  )
  const things = useSWR(
    creativeConceptId ? [ 'tangible-things', creativeConceptId, page ] : null,
    () => listTangibleThings(creativeConceptId ?? '', {
      page,
      limit: PAGE_LIMIT,
      sort: 'name',
    }),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (concept.error || !creativeConceptId) {
    return <AppShell user={user}>
      <p className='relaxed opacity-70'>{
          t('creativeConcepts.notFound')
        }</p>
      <Link to={UrlTree.creativeConcepts} className='text-primary'>{
          t('creativeConcepts.backToList')
        }</Link>
    </AppShell>
  }

  const heldRoles = current?.team.roles ?? []
  const mayWriteThings = can(heldRoles, 'create', TANGIBLE_THING_MODEL)
  const mayDestroyThings = can(heldRoles, 'destroy', TANGIBLE_THING_MODEL)
  const mayDestroyConcept = can(heldRoles, 'destroy', CREATIVE_CONCEPT_MODEL)
  const pagination = things.data?.pagination

  async function onDeleteConcept() {
    await deleteCreativeConcept(creativeConceptId ?? '')
    await navigate(UrlTree.creativeConcepts)
  }

  return <AppShell user={user}>
    <div className='relaxed'>
      <Link to={UrlTree.creativeConcepts} className='text-sm text-primary'>{
          t('creativeConcepts.backToList')
        }</Link>
      <div className='level'>
        <h2 className='title'>{
            concept.data?.creative_concept.name ?? t('common.loading')
          }</h2>
        { mayDestroyConcept
          ? <Button
              color='danger'
              variant='flat'
              onPress={onDeleteConcept}
            >
              <span>{
                  t('creativeConcepts.deleteAction')
                }</span>
            </Button>
          : null
        }
      </div>
      <p className='opacity-70'>{
          t('tangibleThings.subtitle')
        }</p>
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        <Table
          removeWrapper
          aria-label={t('tangibleThings.title')}
        >
          <TableHeader>
            <TableColumn>{
                t('tangibleThings.name')
              }</TableColumn>
            <TableColumn>{
                t('tangibleThings.description')
              }</TableColumn>
            <TableColumn>{
                t('common.actions')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={things.data?.tangible_things ?? []}
            isLoading={things.isLoading}
            emptyContent={things.isLoading ? t('common.loading') : t('tangibleThings.empty')}
          >
            {
              (thing: TangibleThing) => (
                <TableRow key={thing.id}>
                  <TableCell>{
                      thing.name
                    }</TableCell>
                  <TableCell>
                    <span className={thing.description ? undefined : 'opacity-50'}>{
                        thing.description ?? t('tangibleThings.noDescription')
                      }</span>
                  </TableCell>
                  <TableCell>
                    <div className='level-left gap-2'>
                      { mayWriteThings
                        ? <Button
                            size='sm'
                            variant='flat'
                            onPress={() => setEditing(thing)}
                          >
                            <span>{
                                t('common.edit')
                              }</span>
                          </Button>
                        : null
                      }
                      { mayDestroyThings
                        ? <Button
                            size='sm'
                            variant='flat'
                            color='danger'
                            onPress={async () => {
                              await deleteTangibleThing(thing.id)
                              if (editing?.id === thing.id) {
                                setEditing(null)
                              }
                              await things.mutate()
                            }}
                          >
                            <span>{
                                t('common.delete')
                              }</span>
                          </Button>
                        : null
                      }
                    </div>
                  </TableCell>
                </TableRow>
              )
            }
          </TableBody>
        </Table>
        { pagination && pagination.total_pages > 1
          ? <div className='level-center mt-4'>
              <Pagination
                page={pagination.page}
                total={pagination.total_pages}
                onChange={setPage}
                aria-label={t('common.pageOf', {
                  page: pagination.page,
                  totalPages: pagination.total_pages,
                })}
              />
            </div>
          : null
        }
      </CardBody>
    </Card>
    { mayWriteThings
      ? <Card className='relaxed p-2'>
          <CardBody>
            <TangibleThingForm
              creativeConceptId={creativeConceptId}
              editing={editing}
              onDone={() => {
                setEditing(null)
                void things.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
  </AppShell>
}
