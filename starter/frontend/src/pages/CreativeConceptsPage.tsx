// Copyright © 2026 Jalapeno Labs

import type { CreativeConcept } from '../api/routes/creativeConceptRoutes'

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import useSWR from 'swr'
import { useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import {
  Button,
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

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import {
  CREATIVE_CONCEPT_MODEL,
  createCreativeConcept,
  listCreativeConcepts,
} from '../api/routes/creativeConceptRoutes'
import { can } from '../roles.generated'
import { getCreativeConceptUrl } from '../urls'

const PAGE_LIMIT = 10

const createSchema = z.object({
  name: z.string().trim().min(1),
})

type CreateFormValues = z.infer<typeof createSchema>
const resolver = zodResolver(createSchema)

export function CreativeConceptsPage() {
  const { t } = useTranslation()
  const { user } = useCurrentUser()
  const { current } = useTeamContext()

  const [ page, setPage ] = useState(1)
  const [ search, setSearch ] = useState('')
  const [ formError, setFormError ] = useState<string | null>(null)

  const teamId = current?.team.id ?? null
  const concepts = useSWR(
    teamId ? [ 'creative-concepts', teamId, page, search ] : null,
    () => listCreativeConcepts(teamId ?? '', {
      page,
      limit: PAGE_LIMIT,
      sort: 'name',
      name: search.trim() || undefined,
    }),
  )

  const form = useForm<CreateFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      name: '',
    },
  })

  const onCreate = form.handleSubmit(async (data) => {
    if (!teamId) {
      console.debug('creative concept submitted without a selected team')
      return
    }

    setFormError(null)
    try {
      await createCreativeConcept(teamId, { name: data.name.trim() })
      form.reset()
      await concepts.mutate()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setFormError(message ?? t('common.somethingWentWrong'))
    }
  })

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (!current) {
    return <AppShell user={user}>
      <p className='opacity-70'>{
          t('creativeConcepts.noTeamSelected')
        }</p>
    </AppShell>
  }

  const mayCreate = can(current.team.roles, 'create', CREATIVE_CONCEPT_MODEL)
  const pagination = concepts.data?.pagination

  return <AppShell user={user}>
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
        <Table
          removeWrapper
          aria-label={t('creativeConcepts.title')}
        >
          <TableHeader>
            <TableColumn>{
                t('creativeConcepts.name')
              }</TableColumn>
            <TableColumn>{
                t('creativeConcepts.created')
              }</TableColumn>
            <TableColumn>{
                t('common.actions')
              }</TableColumn>
          </TableHeader>
          <TableBody
            items={concepts.data?.creative_concepts ?? []}
            isLoading={concepts.isLoading}
            emptyContent={concepts.isLoading ? t('common.loading') : t('creativeConcepts.empty')}
          >
            {
              (concept: CreativeConcept) => (
                <TableRow key={concept.id}>
                  <TableCell>{
                      concept.name
                    }</TableCell>
                  <TableCell>{
                      new Date(concept.created_at).toLocaleDateString()
                    }</TableCell>
                  <TableCell>
                    <Link
                      to={getCreativeConceptUrl(concept.id)}
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
            <h3 className='compact text-xl font-semibold'>{
                t('creativeConcepts.createTitle')
              }</h3>
            <form onSubmit={onCreate}>
              <div className='level items-start'>
                <Input
                  label={t('creativeConcepts.name')}
                  className='w-full'
                  value={form.watch('name')}
                  onChange={(event) => {
                    form.setValue('name', event.currentTarget.value, {
                      shouldDirty: true,
                      shouldValidate: true,
                    })
                  }}
                />
                <Button
                  type='submit'
                  color='primary'
                  className='shrink-0 self-center'
                  isDisabled={!form.formState.isValid || form.formState.isSubmitting}
                  isLoading={form.formState.isSubmitting}
                >
                  <span>{
                      t('creativeConcepts.createAction')
                    }</span>
                </Button>
              </div>
              { formError
                ? <p className='mt-4 text-danger'>{
                    formError
                  }</p>
                : null
              }
            </form>
          </CardBody>
        </Card>
      : null
    }
  </AppShell>
}
