// Copyright © 2026 Jalapeno Labs

import type { TotpEnrollment } from '@jalapenolabs/anubis'

// Core
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import {
  Button,
  Code,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
  Snippet,
  Spinner,
} from '@heroui/react'
import { TextField } from '@jalapenolabs/anubis'

// Utility
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const confirmSchema = z.object({
  code: z.string().trim().min(6),
})

type ConfirmFormValues = z.infer<typeof confirmSchema>
const resolver = zodResolver(confirmSchema)

type Props = {
  isOpen: boolean
  /** Called on close, with whether an enrollment was confirmed. */
  onClose: (didEnroll: boolean) => void
}

/**
 * The two-step TOTP enrollment: scan a secret, then prove it works.
 *
 * The recovery codes are shown once, on the step after confirmation, because
 * the server stores only their hashes. Closing before reading them is the
 * user's decision to make, so the last step says so.
 */
export function TotpEnrollmentModal(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()

  const [ enrollment, setEnrollment ] = useState<TotpEnrollment | null>(null)
  const [ startedAt, setStartedAt ] = useState(0)
  const [ recoveryCodes, setRecoveryCodes ] = useState<string[] | null>(null)
  const [ startError, setStartError ] = useState<string | null>(null)
  const [ didCopy, setDidCopy ] = useState(false)

  const form = useForm<ConfirmFormValues>({
    resolver,
    mode: 'onChange',
    defaultValues: {
      code: '',
    },
  })

  const { reset } = form
  const isOpen = props.isOpen
  useEffect(() => {
    let cancelled = false

    // Every opening starts a fresh enrollment, so a modal closed halfway
    // through never leaves a stale secret on screen.
    if (isOpen) {
      setEnrollment(null)
      setRecoveryCodes(null)
      setStartError(null)
      setDidCopy(false)
      reset({ code: '' })

      api.startTotpEnrollment()
        .then((started) => {
          if (!cancelled) {
            setEnrollment(started)
            // The QR endpoint's URL never changes, so a fresh enrollment needs
            // a fresh query string or the browser shows the previous secret.
            setStartedAt(Date.now())
          }
        })
        .catch((error: unknown) => {
          console.debug('starting the two-factor enrollment failed', error)
          if (!cancelled) {
            setStartError(getApiErrorMessage(error) ?? t('common.somethingWentWrong'))
          }
        })
    }

    return () => {
      cancelled = true
    }
  }, [ isOpen, api, reset, t ])

  const onConfirm = form.handleSubmit(async (data) => {
    form.clearErrors('root')
    try {
      const codes = await api.confirmTotpEnrollment(data.code.trim())
      setRecoveryCodes(codes)
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      form.setError('root', { message: message ?? t('common.somethingWentWrong') })
    }
  })

  async function onCopyRecoveryCodes() {
    if (!recoveryCodes) {
      console.debug('the copy action ran with no recovery codes to copy')
      return
    }

    try {
      await navigator.clipboard.writeText(recoveryCodes.join('\n'))
      setDidCopy(true)
    }
    catch (error) {
      console.debug('copying the recovery codes to the clipboard failed', error)
    }
  }

  return <Modal
    isOpen={props.isOpen}
    onClose={() => props.onClose(Boolean(recoveryCodes))}
    size='lg'
  >
    <ModalContent>
      <ModalHeader>{
          recoveryCodes
            ? t('settings.security.twoFactor.recoveryTitle')
            : t('settings.security.twoFactor.enrollTitle')
        }</ModalHeader>
      <ModalBody>
        { recoveryCodes
          ? <div>
              <p className='relaxed opacity-80'>{
                  t('settings.security.twoFactor.recoveryBody')
                }</p>
              <div className='relaxed grid grid-cols-2 gap-2'>
                {
                  recoveryCodes.map((code) => (
                    <Code key={code} className='text-center'>{
                        code
                      }</Code>
                  ))
                }
              </div>
              <Button
                variant='flat'
                className='w-full'
                onPress={onCopyRecoveryCodes}
              >
                <span>{
                    didCopy
                      ? t('settings.security.twoFactor.recoveryCopied')
                      : t('settings.security.twoFactor.recoveryCopy')
                  }</span>
              </Button>
            </div>
          : null
        }
        { !recoveryCodes && startError
          ? <p className='text-danger'>{
              startError
            }</p>
          : null
        }
        { !recoveryCodes && !startError && !enrollment
          ? <div className='level-center py-6'>
              <Spinner size='sm' />
            </div>
          : null
        }
        { !recoveryCodes && enrollment
          ? <div>
              <p className='relaxed opacity-80'>{
                  t('settings.security.twoFactor.enrollBody')
                }</p>
              <div className='relaxed level-center'>
                <img
                  src={`${api.totpQrUrl()}?enrollment=${startedAt}`}
                  alt={t('settings.security.twoFactor.qrAlt')}
                  className='h-56 w-56 rounded bg-white p-2'
                />
              </div>
              <div className='relaxed'>
                <p className='compact opacity-70'>{
                    t('settings.security.twoFactor.manualEntry')
                  }</p>
                <Snippet
                  hideSymbol
                  variant='bordered'
                  className='w-full'
                  codeString={enrollment.secret}
                >{
                    enrollment.secret
                  }</Snippet>
              </div>
              <form onSubmit={onConfirm}>
                <TextField
                  control={form.control}
                  name='code'
                  label={t('settings.security.twoFactor.codeLabel')}
                  help={t('settings.security.twoFactor.codeHelp')}
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
              </form>
            </div>
          : null
        }
      </ModalBody>
      <ModalFooter>
        { recoveryCodes
          ? <Button
              color='primary'
              onPress={() => props.onClose(true)}
            >
              <span>{
                  t('settings.security.twoFactor.recoveryDone')
                }</span>
            </Button>
          : <>
              <Button
                variant='flat'
                onPress={() => props.onClose(false)}
              >
                <span>{
                    t('common.cancel')
                  }</span>
              </Button>
              <Button
                color='primary'
                isDisabled={!enrollment || !form.formState.isValid || form.formState.isSubmitting}
                isLoading={form.formState.isSubmitting}
                onPress={() => onConfirm()}
              >
                <span>{
                    t('settings.security.twoFactor.confirmAction')
                  }</span>
              </Button>
            </>
        }
      </ModalFooter>
    </ModalContent>
  </Modal>
}
