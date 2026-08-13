// Copyright © 2026 Jalapeno Labs

// Core
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import { EmailField, TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const requestSchema = z.object({
  email: z.email(),
})

const verifySchema = z.object({
  code: z.string().trim().min(6),
})

type RequestFormValues = z.infer<typeof requestSchema>
type VerifyFormValues = z.infer<typeof verifySchema>
const requestResolver = zodResolver(requestSchema)
const verifyResolver = zodResolver(verifySchema)

type Props = {
  /** Called once a session exists; the guest guard does the redirecting. */
  onSignedIn: () => Promise<void>
  /** Called when the account answers with a second-factor challenge instead. */
  onMfaRequired: (mfaToken: string) => void
}

/**
 * Passwordless sign-in: ask for a code, then enter it.
 *
 * The request answers the same way for addresses with no account, so this
 * screen always advances to the code step. Saying "check your inbox" to
 * someone who has no account is the point, not a bug.
 */
export function SignInEmailCodeForm(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const [ sentToAddress, setSentToAddress ] = useState<string | null>(null)

  const requestForm = useForm<RequestFormValues>({
    resolver: requestResolver,
    mode: 'onChange',
    defaultValues: {
      email: '',
    },
  })

  const verifyForm = useForm<VerifyFormValues>({
    resolver: verifyResolver,
    mode: 'onChange',
    defaultValues: {
      code: '',
    },
  })

  const onRequest = requestForm.handleSubmit(async (data) => {
    requestForm.clearErrors('root')
    try {
      await api.requestEmailSignInCode(data.email.trim())
      setSentToAddress(data.email.trim())
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      requestForm.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  const onVerify = verifyForm.handleSubmit(async (data) => {
    verifyForm.clearErrors('root')
    if (!sentToAddress) {
      console.debug('the email-code verification ran before a code was requested')
      return
    }

    try {
      const result = await api.verifyEmailSignInCode(sentToAddress, data.code.trim())
      if (result.status === 'mfa-required') {
        props.onMfaRequired(result.mfaToken)
        return
      }
      await props.onSignedIn()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      verifyForm.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  if (!sentToAddress) {
    return <form onSubmit={onRequest}>
      <p className='compact opacity-70'>{
          t('auth.emailCode.requestBody')
        }</p>
      <EmailField
        control={requestForm.control}
        name='email'
        label={t('common.email')}
        autoComplete='email'
        autoFocus
        isRequired
      />
      { requestForm.formState.errors.root
        ? <p className='compact text-danger'>{
            requestForm.formState.errors.root.message
          }</p>
        : null
      }
      <Button
        type='submit'
        color='primary'
        className='mt-6 w-full'
        isDisabled={!requestForm.formState.isValid || requestForm.formState.isSubmitting}
        isLoading={requestForm.formState.isSubmitting}
      >
        <span>{
            t('auth.emailCode.requestAction')
          }</span>
      </Button>
    </form>
  }

  return <form onSubmit={onVerify}>
    <p className='compact opacity-70'>{
        t('auth.emailCode.sent', { email: sentToAddress })
      }</p>
    <TextField
      control={verifyForm.control}
      name='code'
      label={t('auth.emailCode.codeLabel')}
      autoComplete='one-time-code'
      autoFocus
      isRequired
    />
    { verifyForm.formState.errors.root
      ? <p className='compact text-danger'>{
          verifyForm.formState.errors.root.message
        }</p>
      : null
    }
    <Button
      type='submit'
      color='primary'
      className='mt-6 w-full'
      isDisabled={!verifyForm.formState.isValid || verifyForm.formState.isSubmitting}
      isLoading={verifyForm.formState.isSubmitting}
    >
      <span>{
          t('auth.emailCode.verifyAction')
        }</span>
    </Button>
    <Button
      variant='light'
      className='mt-2 w-full'
      isDisabled={verifyForm.formState.isSubmitting}
      onPress={() => setSentToAddress(null)}
    >
      <span>{
          t('auth.emailCode.useAnotherAddress')
        }</span>
    </Button>
  </form>
}
