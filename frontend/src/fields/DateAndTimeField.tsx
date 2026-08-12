// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * The `date_and_time_field` scaffolder type: an instant, held as an RFC 3339
 * UTC string and edited in the viewer's own timezone.
 *
 * The API speaks UTC and the browser control speaks local time, so the field
 * owns the conversion in both directions. That keeps the timezone question
 * answered once, here, instead of in every generated form.
 */
export function DateAndTimeField<Values extends FieldValues>(props: AnubisFieldProps<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Input
      id={fieldId}
      ref={field.ref}
      name={field.name}
      type='datetime-local'
      aria-label={props.label}
      className='w-full'
      autoFocus={props.autoFocus}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={toLocalDateTimeInput(field.value)}
      onValueChange={(value) => {
        field.onChange(toUtcTimestamp(value))
      }}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}

/**
 * Renders a stored timestamp as the `YYYY-MM-DDTHH:mm` a `datetime-local` input
 * expects, in the viewer's timezone. Unparseable values render empty.
 */
export function toLocalDateTimeInput(storedValue: unknown): string {
  if (storedValue == null || storedValue === '') {
    return ''
  }

  const instant = new Date(String(storedValue))
  if (Number.isNaN(instant.getTime())) {
    console.debug('DateAndTimeField received an unparseable timestamp, rendering it empty', storedValue)
    return ''
  }

  const localInstant = new Date(instant.getTime() - instant.getTimezoneOffset() * 60_000)
  return localInstant.toISOString().slice(0, 16)
}

/**
 * Turns the local `YYYY-MM-DDTHH:mm` a `datetime-local` input produces into the
 * UTC timestamp the API stores. An empty or unparseable control clears the
 * value.
 */
export function toUtcTimestamp(inputValue: string): string | null {
  if (!inputValue) {
    return null
  }

  const instant = new Date(inputValue)
  if (Number.isNaN(instant.getTime())) {
    console.debug('DateAndTimeField could not parse the control value, clearing the field', inputValue)
    return null
  }

  return instant.toISOString()
}
