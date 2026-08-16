// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'

type Props = {
  /** Seconds left on the wait, from `useRetryCountdown`. */
  remaining: number
}

/**
 * The live "try again in Ns" line a rate-limited form shows.
 *
 * The countdown itself lives in the framework package, which stays free of
 * i18next; the wording is the application's, so it lives here. Nothing renders
 * once the wait is over, which is the same moment the form's submit comes
 * back.
 */
export function RateLimitNotice(props: Props) {
  const { t } = useTranslation()

  if (props.remaining <= 0) {
    return null
  }

  return <p role='alert' className='compact text-danger'>{
      t('auth.rateLimited', { count: props.remaining })
    }</p>
}
