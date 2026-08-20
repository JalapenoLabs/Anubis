// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'

// Core
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser, getApiErrorMessage } from '@jalapenolabs/anubis'

// UI
import { Avatar, Button, Card, CardBody } from '@heroui/react'

/** The upload limit the backend enforces, mirrored so a picker fails fast. */
const MAX_AVATAR_BYTES = 5 * 1024 * 1024

const ACCEPTED_IMAGE_TYPES = 'image/jpeg,image/png,image/webp,image/gif'

type Props = {
  user: User
}

/**
 * The account's picture: preview, upload, remove.
 *
 * There is no cropper here on purpose. The server center-crops to a square,
 * resizes to 512 px, flattens transparency, and re-encodes as JPEG, so a
 * cropper in the browser would only be a second opinion the stored image
 * ignores.
 *
 * Every write refreshes the profile rather than busting the URL locally: the
 * payload carries the avatar's version, so one refresh updates this card and
 * the navbar together.
 */
export function AvatarCard(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()

  const fileInputRef = useRef<HTMLInputElement>(null)
  const [ selectedFile, setSelectedFile ] = useState<File | null>(null)
  const [ isSaving, setIsSaving ] = useState(false)
  const [ isRemoving, setIsRemoving ] = useState(false)
  const [ errorMessage, setErrorMessage ] = useState<string | null>(null)

  const previewUrl = useMemo(
    () => selectedFile && URL.createObjectURL(selectedFile),
    [ selectedFile ],
  )
  useEffect(() => {
    return () => {
      if (previewUrl) {
        URL.revokeObjectURL(previewUrl)
      }
    }
  }, [ previewUrl ])

  function onFileChosen(file: File | undefined) {
    setErrorMessage(null)

    if (!file) {
      console.debug('the avatar picker closed without a file')
      setSelectedFile(null)
      return
    }
    if (file.size > MAX_AVATAR_BYTES) {
      console.debug('the chosen avatar is over the upload limit', file.size)
      setSelectedFile(null)
      setErrorMessage(t('settings.profile.avatarTooLarge'))
      return
    }

    setSelectedFile(file)
  }

  async function onUpload() {
    if (!selectedFile) {
      console.debug('the avatar upload ran with no file selected')
      return
    }

    setIsSaving(true)
    setErrorMessage(null)
    try {
      await api.uploadAvatar(selectedFile)
      setSelectedFile(null)
      await refresh()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsSaving(false)
    }
  }

  async function onRemove() {
    setIsRemoving(true)
    setErrorMessage(null)
    try {
      await api.deleteAvatar()
      setSelectedFile(null)
      await refresh()
    }
    catch (error) {
      const message = getApiErrorMessage(error)
      setErrorMessage(message ?? t('common.somethingWentWrong'))
    }
    finally {
      setIsRemoving(false)
    }
  }

  const isBusy = isSaving || isRemoving

  return <Card className='relaxed p-2'>
    <CardBody>
      <h3 className='compact text-xl font-semibold'>{
          t('settings.profile.avatarTitle')
        }</h3>
      <div className='level-left items-start'>
        <Avatar
          size='lg'
          showFallback
          className='h-24 w-24 shrink-0'
          src={previewUrl ?? api.avatarUrl(props.user)}
          name={props.user.email.slice(0, 2).toUpperCase()}
        />
        <div className='w-full'>
          <p className='compact opacity-70'>{
              t('settings.profile.avatarHelp')
            }</p>
          <input
            ref={fileInputRef}
            type='file'
            className='hidden'
            accept={ACCEPTED_IMAGE_TYPES}
            onChange={(event) => onFileChosen(event.currentTarget.files?.[0])}
          />
          <div className='level-left gap-2'>
            <Button
              variant='flat'
              isDisabled={isBusy}
              onPress={() => fileInputRef.current?.click()}
            >
              <span>{
                  t('settings.profile.avatarChoose')
                }</span>
            </Button>
            <Button
              color='primary'
              isDisabled={!selectedFile || isBusy}
              isLoading={isSaving}
              onPress={onUpload}
            >
              <span>{
                  t('settings.profile.avatarSave')
                }</span>
            </Button>
            {/* Only an account that has a picture can be offered the way to
                take it away. */}
            { props.user.avatarVersion
              ? <Button
                  variant='light'
                  color='danger'
                  isDisabled={isBusy}
                  isLoading={isRemoving}
                  onPress={onRemove}
                >
                  <span>{
                      t('settings.profile.avatarRemove')
                    }</span>
                </Button>
              : null
            }
          </div>
          { selectedFile
            ? <p className='mt-2 opacity-70'>{
                t('settings.profile.avatarSelected', { name: selectedFile.name })
              }</p>
            : null
          }
          { errorMessage
            ? <p className='mt-2 text-danger'>{
                errorMessage
              }</p>
            : null
          }
        </div>
      </div>
    </CardBody>
  </Card>
}
