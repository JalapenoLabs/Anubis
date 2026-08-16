// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, FileReference, UploadLabels } from '../types'
import type { ChangeEvent } from 'react'
import type { FieldValues } from 'react-hook-form'

// Core
import { useRef, useState } from 'react'

// User interface
import { Button } from '@heroui/react'
import { FieldWrapper } from '../FieldWrapper'

// Misc
import { useFieldState } from '../useFieldState'

/** The units `formatBytes` counts in, smallest first. */
const BYTE_UNITS = [ 'B', 'KB', 'MB', 'GB' ]

/**
 * The English fallbacks, deliberately free of the words "file" and "image".
 *
 * The label above the control already says which of the two this is, so one
 * set of defaults serves both fields and a form that translates them writes
 * the noun itself.
 */
export const DEFAULT_UPLOAD_LABELS: UploadLabels = {
  choose: 'Choose',
  replace: 'Replace',
  remove: 'Remove',
  uploading: 'Uploading',
  tooLarge: 'That is larger than {max}.',
  failed: 'The upload did not finish. Try again.',
}

/**
 * A byte count as a person reads it, e.g. `1.4 MB`.
 *
 * Exported so the size shown beside a chosen file and the size named in a
 * "too large" message are produced by one function and can never disagree.
 */
export function formatBytes(bytes: number): string {
  let size = bytes
  let unit = 0
  while (size >= 1024 && unit < BYTE_UNITS.length - 1) {
    size /= 1024
    unit += 1
  }

  const rounded = unit === 0
    ? String(Math.round(size))
    : size.toFixed(1)
  return `${rounded} ${BYTE_UNITS[unit]}`
}

export type UploadProps<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /**
   * Stores the chosen file and resolves the reference the form holds.
   *
   * The field never talks to a server itself. The application owns its upload
   * endpoint, its storage, and its URLs, and this is where it hands them over.
   */
  onUpload: (file: File) => Promise<FileReference>
  /** The `accept` attribute, e.g. `application/pdf` or `image/*`. */
  accept?: string
  /** Refused before the upload starts, so a large file is never sent. */
  maxBytes?: number
  /** Translated words. Anything left out keeps the English default. */
  labels?: Partial<UploadLabels>
}

type Props<Values extends FieldValues> = UploadProps<Values> & {
  labels: UploadLabels
  /** Renders the stored file as a thumbnail rather than as a link. */
  withPreview: boolean
}

/**
 * The control behind `file_field` and `image`, which differ only in whether
 * the stored file is shown as a thumbnail or as a link.
 *
 * The field is **controlled all the way down**: its value is a
 * [`FileReference`](../types.ts) the application's own endpoint produced, and
 * `onUpload` is the only thing that talks to a server. That is deliberate
 * rather than provisional. Anubis stores avatars as bytes in Postgres, and
 * whether every application's attachments belong there, in S3, or behind a
 * signed CDN URL is a decision an application makes once and lives with; a
 * field component that picked one would be wrong for two thirds of them. The
 * framework's own attachments endpoint, when it lands, is one implementation
 * of `onUpload` and changes nothing here.
 *
 * A failed or oversized upload is a transient problem with the control rather
 * than a verdict on the value, so it is shown in the wrapper's error line
 * without ever reaching react-hook-form's own error state.
 */
export function UploadField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  const inputRef = useRef<HTMLInputElement>(null)
  const [ isUploading, setIsUploading ] = useState(false)
  const [ uploadError, setUploadError ] = useState<string | null>(null)

  // react-hook-form types a field's value from the form's own shape, which a
  // generic control cannot see, so the field's half of the contract is stated
  // once here rather than at each of the five places that reads it.
  const stored = field.value as FileReference | null | undefined
  const isLocked = props.isDisabled || props.isReadOnly

  async function onFileChosen(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0]
    // Reset the input straight away so choosing the same file twice in a row
    // still fires a change event, which is what a retry after a failure is.
    event.target.value = ''
    if (!file) {
      console.debug('UploadField received a change event with no file')
      return
    }

    if (props.maxBytes && file.size > props.maxBytes) {
      setUploadError(props.labels.tooLarge.replace('{max}', formatBytes(props.maxBytes)))
      return
    }

    setUploadError(null)
    setIsUploading(true)
    try {
      field.onChange(await props.onUpload(file))
    }
    catch (error) {
      console.debug('UploadField could not upload the chosen file', error)
      setUploadError(props.labels.failed)
    }
    finally {
      setIsUploading(false)
    }
  }

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={uploadError ?? errorMessage}
    className={props.className}
  >
    <div className='flex items-center gap-3'>
      { stored && props.withPreview
        ? <img
            src={stored.url}
            alt={stored.name ?? ''}
            className='h-16 w-16 rounded-medium object-cover'
          />
        : null
      }
      { stored && !props.withPreview
        ? <a
            href={stored.url}
            target='_blank'
            rel='noreferrer'
            className='text-small text-primary underline'
          >{
            stored.name ?? stored.url
          }</a>
        : null
      }
      { stored?.size
        ? <span className='text-tiny opacity-70'>{
            formatBytes(stored.size)
          }</span>
        : null
      }

      <input
        ref={inputRef}
        id={fieldId}
        name={field.name}
        type='file'
        // `sr-only` rather than `hidden`: the native input stays in the
        // accessibility tree and in the tab order, so the field is operable
        // without ever seeing the button that opens the picker.
        className='sr-only'
        aria-label={props.label}
        accept={props.accept}
        disabled={isLocked}
        onChange={onFileChosen}
        onBlur={field.onBlur}
      />
      <Button
        type='button'
        size='sm'
        variant='flat'
        color={isInvalid || uploadError ? 'danger' : 'default'}
        isDisabled={isLocked}
        isLoading={isUploading}
        onPress={() => inputRef.current?.click()}
      >
        <span>{
          isUploading
            ? props.labels.uploading
            : (stored ? props.labels.replace : props.labels.choose)
        }</span>
      </Button>
      { stored && !isLocked
        ? <Button
            type='button'
            size='sm'
            variant='light'
            onPress={() => {
              setUploadError(null)
              field.onChange(null)
            }}
          >
            <span>{props.labels.remove}</span>
          </Button>
        : null
      }
    </div>
  </FieldWrapper>
}
