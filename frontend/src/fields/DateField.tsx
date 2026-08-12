// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /** Earliest allowed date, as `YYYY-MM-DD`. */
  min?: string
  /** Latest allowed date, as `YYYY-MM-DD`. */
  max?: string
}

/**
 * The `date_field` scaffolder type: a calendar date with no time and no zone.
 *
 * The form value is the `YYYY-MM-DD` string the API speaks, stored verbatim. A
 * calendar date means the same day everywhere, so converting it through a
 * `Date` would only invent a timezone bug.
 */
export function DateField<Values extends FieldValues>(props: Props<Values>) {
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
      type='date'
      aria-label={props.label}
      className='w-full'
      autoFocus={props.autoFocus}
      min={props.min}
      max={props.max}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={field.value == null ? '' : String(field.value)}
      onValueChange={(value) => {
        field.onChange(value === '' ? null : value)
      }}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}
