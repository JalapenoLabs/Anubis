// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  min?: number
  max?: number
  /** The `step` attribute. Use a fraction for decimals; defaults to integers. */
  step?: number
}

/**
 * The `number_field` scaffolder type.
 *
 * The form value is a `number`, or `null` once the control is cleared, so a
 * nullable column round-trips without the empty string ever reaching the API.
 */
export function NumberField<Values extends FieldValues>(props: Props<Values>) {
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
      type='number'
      inputMode='numeric'
      aria-label={props.label}
      className='w-full'
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      min={props.min}
      max={props.max}
      step={props.step}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={field.value == null ? '' : String(field.value)}
      onValueChange={(value) => {
        field.onChange(value === '' ? null : Number(value))
      }}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}
