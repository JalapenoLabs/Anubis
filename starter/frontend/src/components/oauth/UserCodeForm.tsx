// Copyright © 2026 Jalapeno Labs

import type { FormEvent } from 'react'

// Core
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'
import {
  PASSWORD_CHANGE_REQUIRED,
  getApiErrorCode,
  getApiErrorMessage,
  getRetryAfterSeconds,
  useAnubisApi,
  useRetryCountdown,
} from '@jalapenolabs/anubis'

// UI
import { Alert, Button, Input, Tooltip } from '@heroui/react'
import { RateLimitNotice } from '../RateLimitNotice'

// Utility
import { HTTPError } from 'ky'

// Misc
import { UrlTree, getConsentRequestUrl } from '../../urls'

/** A user code is eight letters; people add a dash or spaces while copying it. */
const USER_CODE_LENGTH = 8

type Props = {
  /** The code a device's link carried, or empty when the person types it. */
  initialCode: string
}

/**
 * The one field a person types a device's code into.
 *
 * A code that arrived in the link is shown for the person to check against the
 * device rather than sent at once, which RFC 8628 asks for: the person
 * confirming the device in front of them is the only thing that tells it apart
 * from a code somebody else sent. A valid code opens the consent screen for
 * the request it names.
 */
export function UserCodeForm(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const retry = useRetryCountdown()
  const [ userCode, setUserCode ] = useState(props.initialCode.toUpperCase())
  const [ isSubmitting, setIsSubmitting ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)
  const [ needsOwnPassword, setNeedsOwnPassword ] = useState(false)

  const codeRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    codeRef.current?.focus()
  }, [])

  const isComplete = userCode.replace(/[\s-]/g, '').length === USER_CODE_LENGTH

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setIsSubmitting(true)
    setErrorMessage(null)
    setNeedsOwnPassword(false)
    try {
      const requestId = await api.verifyUserCode(userCode)
      navigate(getConsentRequestUrl(requestId))
    }
    catch (error) {
      // A rate-limited refusal keeps the code on screen with a live countdown:
      // the code may be fine, the pace is not.
      const wait = getRetryAfterSeconds(error)
      if (wait) {
        retry.start(wait)
        return
      }

      console.debug('the user code was refused', error)
      // An account on an operator's temporary password may do nothing else
      // until it chooses its own, and the consent screen says so the same way.
      if (getApiErrorCode(error) === PASSWORD_CHANGE_REQUIRED) {
        setNeedsOwnPassword(true)
        return
      }
      // Every code that names nothing waiting answers one 400, whose server
      // sentence is English; the translated one says the same.
      if (error instanceof HTTPError && error.response.status === 400) {
        setErrorMessage(t('oauth.device.invalid'))
        return
      }
      setErrorMessage(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsSubmitting(false)
    }
  }

  return <form onSubmit={onSubmit}>
    { props.initialCode
      ? <div className='compact'>
          <Alert
            color='warning'
            title={t('oauth.device.checkCode')}
          />
        </div>
      : null
    }
    <div className='compact'>
      <Input
        ref={codeRef}
        id='device-user-code'
        label={t('oauth.device.code')}
        className='w-full'
        classNames={{ input: 'font-mono tracking-widest' }}
        autoComplete='off'
        autoCapitalize='characters'
        spellCheck='false'
        value={userCode}
        onChange={(event) => setUserCode(event.currentTarget.value.toUpperCase())}
      />
    </div>
    <RateLimitNotice remaining={retry.remaining} />
    { errorMessage
      ? <p role='alert' className='compact text-danger'>{
          errorMessage
        }</p>
      : null
    }
    { needsOwnPassword
      ? <div className='compact'>
          <p role='alert' className='compact text-danger'>{
              t('oauth.consent.passwordChangeRequired')
            }</p>
          <Link to={UrlTree.settingsSecurity} className='text-primary'>{
              t('oauth.consent.choosePassword')
            }</Link>
        </div>
      : null
    }
    <div className='relaxed mt-6'>
      <Tooltip
        content={t('oauth.device.codeIncomplete')}
        isDisabled={isComplete}
        placement='top'
      >
        <div>
          <Button
            type='submit'
            color='primary'
            className='w-full'
            isDisabled={!isComplete || isSubmitting || !retry.done}
            isLoading={isSubmitting}
          >
            <span>{
                t('oauth.device.continue')
              }</span>
          </Button>
        </div>
      </Tooltip>
    </div>
  </form>
}
