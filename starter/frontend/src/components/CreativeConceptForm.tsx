// Copyright © 2026 Jalapeno Labs

import type { CreativeConcept } from '../api/routes/creativeConceptRoutes'

// Core
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import { TextAreaField, TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import {
  createCreativeConcept,
  updateCreativeConcept,
} from '../api/routes/creativeConceptRoutes'

const creativeConceptSchema = z.object({
  name: z.string().trim().min(1),
  description: z.string(),
})

type CreativeConceptFormValues = z.infer<typeof creativeConceptSchema>
const resolver = zodResolver(creativeConceptSchema)

type Props = {
  teamId: string
  /** The creative concept being edited, or null to create a new one. */
  editing: CreativeConcept | null
  /** Called after a successful write, and when an edit is abandoned. */
  onDone: () => void
}

/** Creates a creative concept, or edits the one passed in `editing`. */
export function CreativeConceptForm(props: Props) {
  const { t } = useTranslation()

  const form = useForm<CreativeConceptFormValues>({
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
        await updateCreativeConcept(editing.id, {
          name: data.name.trim(),
          description: data.description.trim(),
        })
      }
      else {
        await createCreativeConcept(props.teamId, {
          name: data.name.trim(),
          description: data.description.trim(),
        })
        reset({
          name: '',
          description: '',
        })
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
          ? t('creativeConcepts.editTitle', { name: editing.name })
          : t('creativeConcepts.createTitle')
      }</h3>
    <TextField
      control={form.control}
      name='name'
      label={t('creativeConcepts.name')}
      help={t('creativeConcepts.nameHelp')}
      isRequired
    />
    <TextAreaField
      control={form.control}
      name='description'
      label={t('creativeConcepts.description')}
      help={t('creativeConcepts.descriptionHelp')}
      minRows={2}
    />
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
              : t('creativeConcepts.createAction')
          }</span>
      </Button>
    </div>
  </form>
}
