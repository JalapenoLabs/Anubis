// Copyright © 2026 Jalapeno Labs

import type { CreativeConcept } from '../api/routes/creativeConceptRoutes'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import useSWR from 'swr'
import { useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import {
  Card,
  CardBody,
  Input,
  Pagination,
  Table,
  TableBody,
  TableCell,
  TableColumn,
  TableHeader,
  TableRow,
} from '@heroui/react'
import { AppShell } from '../components/AppShell'
import { CreativeConceptForm } from '../components/CreativeConceptForm'

// Misc
import {
  CREATIVE_CONCEPT_MODEL,
  listCreativeConcepts,
} from '../api/routes/creativeConceptRoutes'
import { can } from '../roles.generated'
import { UrlTree, getCreativeConceptUrl } from '../urls'

const PAGE_LIMIT = 10

export function CreativeConceptsPage() {
  const { t } = useTranslation()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()

  const [ page, setPage ] = useState(1)
  const [ search, setSearch ] = useState('')

  const teamId = current?.team.id ?? null
  const creativeConcepts = useSWR(
    teamId ? [ 'creative-concepts', teamId, page, search ] : null,
    () => listCreativeConcepts(teamId ?? '', {
      page,
      limit: PAGE_LIMIT,
      sort: 'name',
      name: search.trim() || undefined,
    }),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  const breadcrumbs = [
    {
      label: t('app.title'),
      to: UrlTree.root,
    },
    {
      label: t('creativeConcepts.title'),
    },
  ]

  if (!current) {
    return <AppShell user={user} breadcrumbs={breadcrumbs}>
      <p className='opacity-70'>{
          t('creativeConcepts.noTeamSelected')
        }</p>
    </AppShell>
  }

  const mayCreate = can(current.team.roles, 'create', CREATIVE_CONCEPT_MODEL)
  const pagination = creativeConcepts.data?.pagination

  return <AppShell user={user} breadcrumbs={breadcrumbs}>
    <div className='relaxed'>
      <h2 className='title'>{
          t('creativeConcepts.title')
        }</h2>
      <p className='opacity-70'>{
          t('creativeConcepts.subtitle')
        }</p>
    </div>
    <Card className='relaxed p-2'>
      <CardBody>
        <div className='compact'>
          <Input
            isClearable
            label={t('creativeConcepts.searchLabel')}
            className='w-full max-w-sm'
            value={search}
            onChange={(event) => {
              setPage(1)
              setSearch(event.currentTarget.value)
            }}
            onClear={() => {
              setPage(1)
              setSearch('')
            }}
          />
        </div>
        {/* `removeWrapper` drops HeroUI's own scroll container along with its
            card chrome, so the table needs one of its own: without it a narrow
            viewport clips the last columns against the card's edge, and the row
            actions in them cannot be reached at all. */}
        <div className='overflow-x-auto'>
          <Table
            removeWrapper
            aria-label={t('creativeConcepts.title')}
          >
            <TableHeader>
              <TableColumn>{
                  t('creativeConcepts.fields.name')
                }</TableColumn>
              <TableColumn>{
                  t('creativeConcepts.fields.description')
                }</TableColumn>
              {/* 🐺 anubis:list-columns */}
              <TableColumn>{
                  t('creativeConcepts.created')
                }</TableColumn>
              <TableColumn>{
                  t('common.actions')
                }</TableColumn>
            </TableHeader>
            <TableBody
              items={creativeConcepts.data?.creative_concepts ?? []}
              isLoading={creativeConcepts.isLoading}
              emptyContent={
                creativeConcepts.isLoading
                  ? t('common.loading')
                  : t('creativeConcepts.empty')
              }
            >
              {
                (creativeConcept: CreativeConcept) => (
                  <TableRow key={creativeConcept.id}>
                    <TableCell>{
                        creativeConcept.name
                      }</TableCell>
                    <TableCell>
                      {/* Two lines of prose, then an ellipsis: a description
                          written at length would otherwise set the height of
                          every row around it. */}
                      <span className={
                        creativeConcept.description
                          ? 'line-clamp-2'
                          : 'opacity-50'
                      }>{
                          creativeConcept.description ?? t('creativeConcepts.noDescription')
                        }</span>
                    </TableCell>
                    {/* 🐺 anubis:list-cells */}
                    <TableCell>{
                        new Date(creativeConcept.created_at).toLocaleDateString()
                      }</TableCell>
                    <TableCell>
                      <Link
                        to={getCreativeConceptUrl(creativeConcept.id)}
                        className='text-primary'
                      >{
                          t('creativeConcepts.open')
                        }</Link>
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
    { mayCreate
      ? <Card className='relaxed p-2'>
          <CardBody>
            <CreativeConceptForm
              teamId={current.team.id}
              editing={null}
              onDone={() => {
                void creativeConcepts.mutate()
              }}
            />
          </CardBody>
        </Card>
      : null
    }
  </AppShell>
}
