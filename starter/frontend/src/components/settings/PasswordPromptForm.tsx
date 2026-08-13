// Copyright © 2026 Jalapeno Labs

// Core
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import { PasswordField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const passwordPromptSchema = z.object({
  password: z.string().min(1),
})

type PasswordPromptFormValues = z.infer<typeof passwordPromptSchema>
const resolver = zodResolver(passwordPromptSchema)

type Props = {
  submitLabel: string
  /** `danger` for the destructive confirmations; `primary` otherwise. */
  submitColor?: 'primary' | 'danger'
  /** Blocks the submit while the caller's own confirmation is incomplete. */
  isDisabled?: boolean
  onCancel?: () => void
  /** Throws on failure; the thrown API error becomes the form's message. */
  onConfirm: (password: string) => Promise<void>
}

/**
 * The password re-entry every sensitive action asks for.
 *
 * Disabling two-factor auth and deleting an account both prove intent the same
 * way, so they share one form rather than two that drift apart.
 */
export function PasswordPromptForm(props: Props) {
  const { t } = useTranslation()

  const form = useForm<PasswordPromptFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      password: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      await props.onConfirm(data.password)
      form.reset({ password: '' })
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <form onSubmit={onSubmit}>
    <PasswordField
      control={form.control}
      name='password'
      label={t('common.password')}
      autoComplete='current-password'
      autoFocus
      isRequired
    />
    { form.formState.errors.root
      ? <p className='compact text-danger'>{
          form.formState.errors.root.message
        }</p>
      : null
    }
    <div className='level-right mt-4 gap-2'>
      { props.onCancel
        ? <Button
            variant='flat'
            isDisabled={form.formState.isSubmitting}
            onPress={props.onCancel}
          >
            <span>{
                t('common.cancel')
              }</span>
          </Button>
        : null
      }
      <Button
        type='submit'
        color={props.submitColor ?? 'primary'}
        isDisabled={props.isDisabled || !form.formState.isValid || form.formState.isSubmitting}
        isLoading={form.formState.isSubmitting}
      >
        <span>{
            props.submitLabel
          }</span>
      </Button>
    </div>
  </form>
}
