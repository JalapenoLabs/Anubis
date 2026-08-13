// Copyright © 2026 Jalapeno Labs

import type { FieldOption, User } from '@jalapenolabs/anubis'

// Core
import { useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Button, Card, CardBody } from '@heroui/react'
import { OptionsField, SuperSelectField, TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

// Misc
import { i18n } from '../../i18n'

const profileSchema = z.object({
  firstName: z.string(),
  lastName: z.string(),
  timeZone: z.string().min(1),
  locale: z.string().min(1),
})

type ProfileFormValues = z.infer<typeof profileSchema>
const resolver = zodResolver(profileSchema)

/**
 * The locales this application ships, read from the i18next resources.
 *
 * The bundle is the source of truth: a locale the app has no strings for is a
 * locale a user must not be able to pick.
 */
const localeOptions: FieldOption[] = Object.keys(i18n.options.resources ?? {}).map((tag) => ({
  value: tag,
  label: new Intl.DisplayNames([ tag ], { type: 'language' }).of(tag) ?? tag,
}))

type Props = {
  user: User
}

/** Name, time zone, and locale, saved with one `PATCH /auth/profile`. */
export function ProfileForm(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()
  const [ savedMessage, setSavedMessage ] = useState<string | null>(null)

  const timeZoneOptions = useMemo(() => buildTimeZoneOptions(props.user.timeZone), [ props.user.timeZone ])

  const form = useForm<ProfileFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      firstName: props.user.firstName ?? '',
      lastName: props.user.lastName ?? '',
      timeZone: props.user.timeZone,
      locale: props.user.locale,
    },
  })

  const onSubmit = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    setSavedMessage(null)
    try {
      await api.updateProfile({
        firstName: data.firstName.trim(),
        lastName: data.lastName.trim(),
        timeZone: data.timeZone,
        locale: data.locale,
      })
      await refresh()
      await i18n.changeLanguage(data.locale)
      setSavedMessage(t('settings.profile.saved'))
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.profile.detailsTitle')
        }</h3>
      <form onSubmit={onSubmit}>
        <div className='level items-start'>
          <TextField
            control={form.control}
            name='firstName'
            label={t('settings.profile.firstName')}
            className='w-full'
            autoComplete='given-name'
          />
          <TextField
            control={form.control}
            name='lastName'
            label={t('settings.profile.lastName')}
            className='w-full'
            autoComplete='family-name'
          />
        </div>
        <SuperSelectField
          control={form.control}
          name='timeZone'
          label={t('settings.profile.timeZone')}
          help={t('settings.profile.timeZoneHelp')}
          options={timeZoneOptions}
          isRequired
        />
        <OptionsField
          control={form.control}
          name='locale'
          label={t('settings.profile.locale')}
          help={t('settings.profile.localeHelp')}
          options={localeOptions}
          isRequired
        />
        { form.formState.errors.root
          ? <p className='compact text-danger'>{
              form.formState.errors.root.message
            }</p>
          : null
        }
        { savedMessage
          ? <p className='compact opacity-80'>{
              savedMessage
            }</p>
          : null
        }
        <div className='level-right mt-4'>
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
      </form>
    </CardBody>
  </Card>
}

/**
 * Every time zone the browser knows, with the account's own kept selectable.
 *
 * A stored zone the browser does not list (an alias, or a zone from a newer
 * tzdata) would otherwise render the field empty and silently rewrite the
 * account's setting on the next save.
 */
function buildTimeZoneOptions(currentTimeZone: string): FieldOption[] {
  const zones = Intl.supportedValuesOf('timeZone')
  const options: FieldOption[] = zones.map((zone) => ({
    value: zone,
    label: zone.replaceAll('_', ' '),
  }))

  if (!zones.includes(currentTimeZone)) {
    console.debug('the account time zone is unknown to this browser, offering it anyway', currentTimeZone)
    options.unshift({
      value: currentTimeZone,
      label: currentTimeZone.replaceAll('_', ' '),
    })
  }

  return options
}
