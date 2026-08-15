// Copyright © 2026 Jalapeno Labs

// Core
import { useNavigate } from 'react-router'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button } from '@heroui/react'
import { PasswordField } from '@jalapenolabs/anubis'
import { AuthLayout } from '../../components/AuthLayout'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import { POST_SIGN_IN_REDIRECT_TO, UrlTree } from '../../urls'

const forcedPasswordChangeSchema = z.object({
  currentPassword: z.string().min(1),
  newPassword: z.string().min(8),
})

type ForcedPasswordChangeValues = z.infer<typeof forcedPasswordChangeSchema>
const resolver = zodResolver(forcedPasswordChangeSchema)

/**
 * The one screen an account owing a password change can use.
 *
 * An administrator provisioned the credential and asked for it to be replaced,
 * so every other route answers `password_change_required` until the change
 * lands. Signing out is the other way off this page, for the person who cannot
 * produce the current password and needs the reset link instead.
 */
export function ForcePasswordChangePage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const { refresh } = useCurrentUser()

  const form = useForm<ForcedPasswordChangeValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      currentPassword: '',
      newPassword: '',
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      await api.changePassword({
        currentPassword: data.currentPassword,
        newPassword: data.newPassword,
      })
      // The change is what clears the demand, so asking the backend again is
      // what turns this screen back into the application.
      await refresh()
      navigate(POST_SIGN_IN_REDIRECT_TO, { replace: true })
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  async function onSignOut() {
    try {
      await api.logout()
    }
    catch (error) {
      console.debug('logout request failed; refreshing session state anyway', error)
    }
    await refresh()
    navigate(UrlTree.signIn, { replace: true })
  }

  return <AuthLayout
    title={t('auth.forcedPasswordChange.title')}
    subtitle={t('auth.forcedPasswordChange.subtitle')}
  >
    <form onSubmit={onSubmit}>
      <PasswordField
        control={form.control}
        name='currentPassword'
        label={t('auth.forcedPasswordChange.current')}
        autoComplete='current-password'
        isRequired
      />
      <PasswordField
        control={form.control}
        name='newPassword'
        label={t('auth.forcedPasswordChange.new')}
        help={t('auth.validation.passwordTooShort', { count: 8 })}
        autoComplete='new-password'
        isRequired
      />
      { form.formState.errors.root
        ? <p className='compact text-danger'>{
            form.formState.errors.root.message
          }</p>
        : null
      }
      <div className='relaxed mt-6'>
        <Button
          type='submit'
          color='primary'
          className='w-full'
          isDisabled={!form.formState.isValid || form.formState.isSubmitting}
          isLoading={form.formState.isSubmitting}
        >
          <span>{
              t('auth.forcedPasswordChange.action')
            }</span>
        </Button>
      </div>
    </form>
    <div className='level-right mt-4 text-sm'>
      <Button
        size='sm'
        variant='light'
        onPress={onSignOut}
      >
        <span>{
            t('common.signOut')
          }</span>
      </Button>
    </div>
  </AuthLayout>
}
