// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Textarea } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /** Rows shown before the control grows. Defaults to HeroUI's own minimum. */
  minRows?: number
  maxRows?: number
}

/** The `text_area` scaffolder type: multi-line free text. */
export function TextAreaField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Textarea
      id={fieldId}
      ref={field.ref}
      name={field.name}
      aria-label={props.label}
      className='w-full'
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      minRows={props.minRows}
      maxRows={props.maxRows}
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
