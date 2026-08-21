// Copyright © 2026 Jalapeno Labs

import type { GranularDetail } from '../api/routes/granularDetailRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
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
import { GranularDetailForm } from './GranularDetailForm'

// Misc
import {
  GRANULAR_DETAIL_MODEL,
  deleteGranularDetail,
  listGranularDetails,
} from '../api/routes/granularDetailRoutes'
import { can } from '../roles.generated'
import { getGranularDetailUrl } from '../urls'

const PAGE_LIMIT = 10

type Props = {
  tangibleThingId: string
  /** The owning team, for the options a scaffolded association offers. */
  teamId: string
}

/**
 * The granular details of one tangible thing: the table, and the form that
 * writes to it.
 *
 * A nested model lives inside its parent's page, so its whole slice is one
 * component the parent renders, and that stays true however deep the chain
 * runs. It is what lets `anubis scaffold model` add a child to an existing
 * parent page by inserting a single element.
 */
export function GranularDetailsSection(props: Props) {
  const { t } = useTranslation()
  const { current } = useTeamContext()

  const [ page, setPage ] = useState(1)
  const [ editing, setEditing ] = useState<GranularDetail | null>(null)

  const granularDetails = useSWR(
    [ 'granular-details', props.tangibleThingId, page ],
    () => listGranularDetails(props.tangibleThingId, {
      page,
      limit: PAGE_LIMIT,
      sort: 'name',
    }),
  )

  const heldRoles = current?.team.roles ?? []
  const mayWrite = can(heldRoles, 'create', GRANULAR_DETAIL_MODEL)
  const mayDestroy = can(heldRoles, 'destroy', GRANULAR_DETAIL_MODEL)
  const pagination = granularDetails.data?.pagination

  return <section>
    <div className='relaxed'>
      {/* A section inside a page, so a step below the page's own `title`. */}
      <h3 className='subtitle'>{
          t('granularDetails.title')
        }</h3>
      <p className='opacity-70'>{
          t('granularDetails.subtitle')
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
            aria-label={t('granularDetails.title')}
          >
            <TableHeader>
              <TableColumn>{
                  t('granularDetails.fields.name')
                }</TableColumn>
              <TableColumn>{
                  t('granularDetails.fields.description')
                }</TableColumn>
              {/* 🐺 anubis:list-columns */}
              <TableColumn>{
                  t('common.actions')
                }</TableColumn>
            </TableHeader>
            <TableBody
              items={granularDetails.data?.granular_details ?? []}
              isLoading={granularDetails.isLoading}
              emptyContent={
                granularDetails.isLoading
                  ? t('common.loading')
                  : t('granularDetails.empty')
              }
            >
              {
                (granularDetail: GranularDetail) => (
                  <TableRow key={granularDetail.id}>
                    <TableCell>{
                        granularDetail.name
                      }</TableCell>
                    <TableCell>
                      {/* Two lines of prose, then an ellipsis: a description
                          written at length would otherwise set the height of
                          every row around it. */}
                      <span className={
                        granularDetail.description
                          ? 'line-clamp-2'
                          : 'opacity-50'
                      }>{
                          granularDetail.description ?? t('granularDetails.noDescription')
                        }</span>
                    </TableCell>
                    {/* 🐺 anubis:list-cells */}
                    <TableCell>
                      <div className='level-left gap-2'>
                        <Link
                          to={getGranularDetailUrl(granularDetail.id)}
                          className='text-primary'
                        >{
                            t('granularDetails.open')
                          }</Link>
                        { mayWrite
                          ? <Button
                              size='sm'
                              variant='flat'
                              onPress={() => setEditing(granularDetail)}
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
                                await deleteGranularDetail(granularDetail.id)
                                if (editing?.id === granularDetail.id) {
                                  setEditing(null)
                                }
                                await granularDetails.mutate()
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
            <GranularDetailForm
              tangibleThingId={props.tangibleThingId}
              teamId={props.teamId}
              editing={editing}
              onDone={() => {
                setEditing(null)
                void granularDetails.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
  </section>
}
