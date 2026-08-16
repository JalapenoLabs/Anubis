// Copyright © 2026 Jalapeno Labs

// Core
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import {
  useAnubisApi,
  useRetryCountdown,
  getApiErrorMessage,
  getRetryAfterSeconds,
} from '@jalapenolabs/anubis'

// UI
import { Button, Tooltip } from '@heroui/react'
import { EmailField, PasswordField } from '@jalapenolabs/anubis'
import { RateLimitNotice } from '../../components/RateLimitNotice'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const signInSchema = z.object({
  email: z.email(),
  password: z.string().min(8),
})

type SignInFormValues = z.infer<typeof signInSchema>
const resolver = zodResolver(signInSchema)

type Props = {
  /** Called once a session exists; the guest guard does the redirecting. */
  onSignedIn: () => Promise<void>
  /** Called when the account answers with a second-factor challenge instead. */
  onMfaRequired: (mfaToken: string) => void
}

/** Email and password, the sign-in method every account has. */
export function SignInPasswordForm(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const retry = useRetryCountdown()

  const form = useForm<SignInFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      email: '',
      password: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      const result = await api.login(data)
      if (result.status === 'mfa-required') {
        props.onMfaRequired(result.mfaToken)
        return
      }
      await props.onSignedIn()
    }
    catch (error) {
      // A rate-limited refusal gets the live countdown instead of a static
      // message, because the wait is the only thing the user can act on.
      const wait = getRetryAfterSeconds(error)
      if (wait) {
        retry.start(wait)
        return
      }

      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  const isValid = form.formState.isValid

  return <form onSubmit={onSubmit}>
    <EmailField
      control={form.control}
      name='email'
      label={t('common.email')}
      autoComplete='email'
      autoFocus
      isRequired
    />
    <PasswordField
      control={form.control}
      name='password'
      label={t('common.password')}
      autoComplete='current-password'
      isRequired
    />
    { form.formState.errors.root
      ? <p className='compact text-danger'>{
          form.formState.errors.root.message
        }</p>
      : null
    }
    <RateLimitNotice remaining={retry.remaining} />
    <Tooltip
      content={t('auth.validation.fillAllFields')}
      isDisabled={isValid}
      placement='top'
    >
      <div className='relaxed mt-6'>
        <Button
          type='submit'
          color='primary'
          className='w-full'
          isDisabled={!isValid || form.formState.isSubmitting || !retry.done}
          isLoading={form.formState.isSubmitting}
        >
          <span>{
              t('auth.signIn.action')
            }</span>
        </Button>
      </div>
    </Tooltip>
  </form>
}
