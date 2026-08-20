// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Textarea } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * Rows a `text_area` shows before it grows.
 *
 * HeroUI's own minimum is two, which stands barely taller than a single-line
 * input: a form of stacked fields then gives the reader no way to tell which
 * one takes a paragraph. Three rows is the smallest height that reads as
 * multi-line at a glance.
 */
const DEFAULT_MIN_ROWS = 3

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /** Rows shown before the control grows. Defaults to {@link DEFAULT_MIN_ROWS}. */
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
      minRows={props.minRows ?? DEFAULT_MIN_ROWS}
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
