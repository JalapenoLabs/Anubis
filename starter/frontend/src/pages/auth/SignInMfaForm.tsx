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
import { Button } from '@heroui/react'
import { TextField } from '@jalapenolabs/anubis'
import { RateLimitNotice } from '../../components/RateLimitNotice'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const challengeSchema = z.object({
  code: z.string().trim().min(6),
})

type ChallengeFormValues = z.infer<typeof challengeSchema>
const resolver = zodResolver(challengeSchema)

type Props = {
  /** The challenge the sign-in attempt answered with. */
  mfaToken: string
  /** Called once a session exists; the guest guard does the redirecting. */
  onSignedIn: () => Promise<void>
  /** Abandons the challenge and returns to the sign-in methods. */
  onCancel: () => void
}

/**
 * The second-factor step: a code from the authenticator app, or a recovery code.
 *
 * One field takes both, because the server accepts either and asking the user
 * to declare which kind they are holding adds a decision to a step that should
 * only take a paste.
 */
export function SignInMfaForm(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const retry = useRetryCountdown()

  const form = useForm<ChallengeFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      code: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      await api.verifyMfaChallenge(props.mfaToken, data.code.trim())
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

  return <form onSubmit={onSubmit}>
    <p className='compact opacity-70'>{
        t('auth.mfa.body')
      }</p>
    <TextField
      control={form.control}
      name='code'
      label={t('auth.mfa.codeLabel')}
      help={t('auth.mfa.codeHelp')}
      autoComplete='one-time-code'
      autoFocus
      isRequired
    />
    { form.formState.errors.root
      ? <p className='compact text-danger'>{
          form.formState.errors.root.message
        }</p>
      : null
    }
    <RateLimitNotice remaining={retry.remaining} />
    <Button
      type='submit'
      color='primary'
      className='mt-6 w-full'
      isDisabled={!form.formState.isValid || form.formState.isSubmitting || !retry.done}
      isLoading={form.formState.isSubmitting}
    >
      <span>{
          t('auth.mfa.action')
        }</span>
    </Button>
    <Button
      variant='light'
      className='mt-2 w-full'
      isDisabled={form.formState.isSubmitting}
      onPress={props.onCancel}
    >
      <span>{
          t('common.cancel')
        }</span>
    </Button>
  </form>
}
