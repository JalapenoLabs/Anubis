// Copyright © 2026 Jalapeno Labs

import type { TangibleThing } from '../api/routes/tangibleThingRoutes'

// Core
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import {
  TextAreaField,
  TextField,
  // 🐺 anubis:field-imports
} from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import {
  createTangibleThing,
  updateTangibleThing,
} from '../api/routes/tangibleThingRoutes'

const tangibleThingSchema = z.object({
  name: z.string().trim().min(1),
  description: z.string(),
  // 🐺 anubis:form-schema
})

type TangibleThingFormValues = z.infer<typeof tangibleThingSchema>
const resolver = zodResolver(tangibleThingSchema)

/**
 * The form's values for one tangible thing, or empty values for a new one.
 *
 * One definition serves the initial values, the reset when the edited record
 * changes, and the reset after a create, so a column added by `anubis scaffold
 * field` reaches all three at once.
 */
function toFormValues(
  editing: TangibleThing | null,
): TangibleThingFormValues {
  return {
    name: editing?.name ?? '',
    description: editing?.description ?? '',
    // 🐺 anubis:form-values
  }
}

type Props = {
  creativeConceptId: string
  /** The tangible thing being edited, or null to create a new one. */
  editing: TangibleThing | null
  /** Called after a successful write, and when an edit is abandoned. */
  onDone: () => void
}

/** Creates a tangible thing, or edits the one passed in `editing`. */
export function TangibleThingForm(props: Props) {
  const { t } = useTranslation()

  const form = useForm<TangibleThingFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: toFormValues(props.editing),
  })

  const { reset } = form
  const editing = props.editing
  useEffect(() => {
    reset(toFormValues(editing))
  }, [ editing, reset ])

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    const payload = {
      name: data.name.trim(),
      description: data.description.trim(),
      // 🐺 anubis:form-payload
    }
    try {
      if (editing) {
        await updateTangibleThing(editing.id, payload)
      }
      else {
        await createTangibleThing(props.creativeConceptId, payload)
        reset(toFormValues(null))
      }
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
    <TextField
      control={form.control}
      name='name'
      label={t('tangibleThings.fields.name')}
      help={t('tangibleThings.fields.nameHelp')}
      isRequired
    />
    <TextAreaField
      control={form.control}
      name='description'
      label={t('tangibleThings.fields.description')}
      help={t('tangibleThings.fields.descriptionHelp')}
      minRows={2}
    />
    {/* 🐺 anubis:form-fields */}
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
