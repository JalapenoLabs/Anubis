// Copyright © 2026 Jalapeno Labs

import type { TangibleThing } from '../api/routes/tangibleThingRoutes'

// Core
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Input, Textarea } from '@heroui/react'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import { createTangibleThing, updateTangibleThing } from '../api/routes/tangibleThingRoutes'

const thingSchema = z.object({
  name: z.string().trim().min(1),
  description: z.string(),
})

type ThingFormValues = z.infer<typeof thingSchema>
const resolver = zodResolver(thingSchema)

type Props = {
  creativeConceptId: string
  /** The thing being edited, or null to create a new one. */
  editing: TangibleThing | null
  /** Called after a successful write, and when an edit is abandoned. */
  onDone: () => void
}

/** Creates a tangible thing, or edits the one passed in `editing`. */
export function TangibleThingForm(props: Props) {
  const { t } = useTranslation()

  const form = useForm<ThingFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      name: '',
      description: '',
    },
  })

  const { reset } = form
  const editing = props.editing
  useEffect(() => {
    reset({
      name: editing?.name ?? '',
      description: editing?.description ?? '',
    })
  }, [ editing, reset ])

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      if (editing) {
        await updateTangibleThing(editing.id, {
          name: data.name.trim(),
          description: data.description.trim(),
        })
      }
      else {
        await createTangibleThing(props.creativeConceptId, {
          name: data.name.trim(),
          description: data.description.trim(),
        })
      }
      reset({
        name: '',
        description: '',
      })
      props.onDone()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <form onSubmit={onSubmit}>
    <h3 className='compact text-xl font-semibold'>{
        editing
          ? t('tangibleThings.editTitle', { name: editing.name })
          : t('tangibleThings.createTitle')
      }</h3>
    <div className='compact'>
      <Input
        label={t('tangibleThings.name')}
        className='w-full'
        value={form.watch('name')}
        onChange={(event) => {
          form.setValue('name', event.currentTarget.value, {
            shouldDirty: true,
            shouldValidate: true,
          })
        }}
      />
    </div>
    <div className='compact'>
      <Textarea
        label={t('tangibleThings.description')}
        // The hint only means something once there is a value to clear.
        description={editing ? t('tangibleThings.descriptionHelp') : undefined}
        className='w-full'
        minRows={2}
        value={form.watch('description')}
        onChange={(event) => {
          form.setValue('description', event.currentTarget.value, {
            shouldDirty: true,
            shouldValidate: true,
          })
        }}
      />
    </div>
    { form.formState.errors.root
      ? <p className='compact text-danger'>{
          form.formState.errors.root.message
        }</p>
      : null
    }
    <div className='level-right mt-4 gap-2'>
      { editing
        ? <Button
            variant='flat'
            onPress={props.onDone}
          >
            <span>{
                t('common.cancel')
              }</span>
          </Button>
        : null
      }
      <Button
        type='submit'
        color='primary'
        isDisabled={!form.formState.isValid || form.formState.isSubmitting}
        isLoading={form.formState.isSubmitting}
      >
        <span>{
            editing
              ? t('common.save')
              : t('tangibleThings.createAction')
          }</span>
      </Button>
    </div>
  </form>
}
