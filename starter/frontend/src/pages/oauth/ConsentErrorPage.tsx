// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'

// UI
import { AuthLayout } from '../../components/AuthLayout'

type Props = {
  /** The machine-readable code the backend redirected with. */
  code: string
}

/**
 * Why a connection stopped before anything could be sent back to the program.
 *
 * The backend lands here instead of on the program's redirect address when it
 * could not trust that address, or the program itself, because sending a
 * browser to an unverified address is an open redirect. No sign-in is needed
 * to read it: nothing here is the person's.
 */
export function ConsentErrorPage(props: Props) {
  const { t } = useTranslation()

  const key = `oauth.errors.${props.code}`
  const message = t(key, { defaultValue: t('oauth.errors.unknown') })

  return <AuthLayout
    title={t('oauth.errors.title')}
    subtitle=''
  >
    <p>{
        message
      }</p>
  </AuthLayout>
}
