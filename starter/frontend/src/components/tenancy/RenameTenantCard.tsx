// Copyright © 2026 Jalapeno Labs

// Core
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

/** The backend bounds a tenant name at 100 characters; say so before it does. */
const renameSchema = z.object({
  name: z.string().trim().min(1).max(100),
})

type RenameFormValues = z.infer<typeof renameSchema>
const resolver = zodResolver(renameSchema)

type Props = {
  title: string
  subtitle: string
  label: string
  /** The tenant's name today, which the field starts on. */
  name: string
  /** False for members without the admin role, who read the name instead. */
  canRename: boolean
  onRename: (name: string) => Promise<void>
}

/**
 * Renames a team or an organization. Both are one field and one button.
 *
 * A member without the admin role still sees the name, disabled: hiding it
 * would answer "what is this team called" with silence, and the server refuses
 * the write regardless of what the screen offers.
 */
export function RenameTenantCard(props: Props) {
  const { t } = useTranslation()
  const [ isSaved, setIsSaved ] = useState(false)

  const form = useForm<RenameFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      name: props.name,
    },
  })

  // The name changes underneath the form when the user switches tenants, or
  // when a rename lands; the field follows what the server has.
  const { reset } = form
  useEffect(() => {
    reset({ name: props.name })
  }, [ props.name, reset ])

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    setIsSaved(false)
    try {
      await props.onRename(data.name.trim())
      setIsSaved(true)
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          props.title
        }</h3>
      <p className='compact opacity-70'>{
          props.subtitle
        }</p>
      <form onSubmit={onSubmit}>
        <TextField
          control={form.control}
          name='name'
          label={props.label}
          isDisabled={!props.canRename}
          isRequired
        />
        { form.formState.errors.root
          ? <p className='compact text-danger'>{
              form.formState.errors.root.message
            }</p>
          : null
        }
        { isSaved
          ? <p className='compact opacity-80'>{
              t('tenancy.renameSaved')
            }</p>
          : null
        }
        { props.canRename
          ? <div className='level-right mt-4'>
              <Button
                type='submit'
                color='primary'
                isDisabled={!form.formState.isValid || form.formState.isSubmitting}
                isLoading={form.formState.isSubmitting}
              >
                <span>{
                    t('common.save')
                  }</span>
              </Button>
            </div>
          : null
        }
      </form>
    </CardBody>
  </Card>
}
