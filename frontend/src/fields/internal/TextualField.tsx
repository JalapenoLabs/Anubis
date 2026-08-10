// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from '../types'
import type { FieldValues } from 'react-hook-form'
import type { HTMLInputTypeAttribute } from 'react'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from '../FieldWrapper'

// Misc
import { useFieldState } from '../useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  type: HTMLInputTypeAttribute
  inputMode?: 'text' | 'email' | 'tel'
  autoComplete?: string
}

/**
 * The single-line text control behind `text_field`, `email_field`,
 * `password_field`, and `phone_field`.
 *
 * Not exported from the package: the four public fields differ only in the
 * input type and the autofill hints they default to, and naming them after the
 * scaffolder's field types is what makes generated forms readable.
 */
export function TextualField<Values extends FieldValues>(props: Props<Values>) {
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
      type={props.type}
      inputMode={props.inputMode}
      autoComplete={props.autoComplete}
      aria-label={props.label}
      className='w-full'
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={field.value == null ? '' : String(field.value)}
      onValueChange={field.onChange}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}
