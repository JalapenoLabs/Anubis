// Copyright © 2026 Jalapeno Labs

import type { TangibleThing } from '../api/routes/tangibleThingRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
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
import { TangibleThingForm } from './TangibleThingForm'

// Misc
import {
  TANGIBLE_THING_MODEL,
  deleteTangibleThing,
  listTangibleThings,
} from '../api/routes/tangibleThingRoutes'
import { can } from '../roles.generated'

const PAGE_LIMIT = 10

type Props = {
  creativeConceptId: string
  /** The owning team, for the options a scaffolded association offers. */
  teamId: string
}

/**
 * The tangible things of one creative concept: the table, and the form that
 * writes to it.
 *
 * A nested model lives inside its parent's page, so its whole slice is one
 * component the parent renders. That is what lets `anubis scaffold model` add a
 * child to an existing parent page by inserting a single element.
 */
export function TangibleThingsSection(props: Props) {
  const { t } = useTranslation()
  const { current } = useTeamContext()

  const [ page, setPage ] = useState(1)
  const [ editing, setEditing ] = useState<TangibleThing | null>(null)

  const tangibleThings = useSWR(
    [ 'tangible-things', props.creativeConceptId, page ],
    () => listTangibleThings(props.creativeConceptId, {
      page,
      limit: PAGE_LIMIT,
      sort: 'name',
    }),
  )

  const heldRoles = current?.team.roles ?? []
  const mayWrite = can(heldRoles, 'create', TANGIBLE_THING_MODEL)
  const mayDestroy = can(heldRoles, 'destroy', TANGIBLE_THING_MODEL)
  const pagination = tangibleThings.data?.pagination

  return <section>
    <div className='relaxed'>
      {/* A section inside a page, so a step below the page's own `title`. */}
      <h3 className='subtitle'>{
          t('tangibleThings.title')
        }</h3>
      <p className='opacity-70'>{
          t('tangibleThings.subtitle')
        }</p>
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        {/* `removeWrapper` drops HeroUI's own scroll container along with its
            card chrome, so the table needs one of its own: without it a narrow
            viewport clips the last columns against the card's edge, and the row
            actions in them cannot be reached at all. */}
        <div className='overflow-x-auto'>
          <Table
            removeWrapper
            aria-label={t('tangibleThings.title')}
          >
            <TableHeader>
              <TableColumn>{
                  t('tangibleThings.fields.name')
                }</TableColumn>
              <TableColumn>{
                  t('tangibleThings.fields.description')
                }</TableColumn>
              {/* 🐺 anubis:list-columns */}
              <TableColumn>{
                  t('common.actions')
                }</TableColumn>
            </TableHeader>
            <TableBody
              items={tangibleThings.data?.tangible_things ?? []}
              isLoading={tangibleThings.isLoading}
              emptyContent={
                tangibleThings.isLoading
                  ? t('common.loading')
                  : t('tangibleThings.empty')
              }
            >
              {
                (tangibleThing: TangibleThing) => (
                  <TableRow key={tangibleThing.id}>
                    <TableCell>{
                        tangibleThing.name
                      }</TableCell>
                    <TableCell>
                      {/* Two lines of prose, then an ellipsis: a description
                          written at length would otherwise set the height of
                          every row around it. */}
                      <span className={
                        tangibleThing.description
                          ? 'line-clamp-2'
                          : 'opacity-50'
                      }>{
                          tangibleThing.description ?? t('tangibleThings.noDescription')
                        }</span>
                    </TableCell>
                    {/* 🐺 anubis:list-cells */}
                    <TableCell>
                      <div className='level-left gap-2'>
                        { mayWrite
                          ? <Button
                              size='sm'
                              variant='flat'
                              onPress={() => setEditing(tangibleThing)}
                            >
                              <span>{
                                  t('common.edit')
                                }</span>
                            </Button>
                          : null
                        }
                        { mayDestroy
                          ? <Button
                              size='sm'
                              variant='flat'
                              color='danger'
                              onPress={async () => {
                                await deleteTangibleThing(tangibleThing.id)
                                if (editing?.id === tangibleThing.id) {
                                  setEditing(null)
                                }
                                await tangibleThings.mutate()
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
        </div>
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
    { mayWrite
      ? <Card className='relaxed p-2'>
          <CardBody>
            <TangibleThingForm
              creativeConceptId={props.creativeConceptId}
              teamId={props.teamId}
              editing={editing}
              onDone={() => {
                setEditing(null)
                void tangibleThings.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
  </section>
}
