// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Switch } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * The `boolean` scaffolder type, rendered as a HeroUI Switch.
 *
 * A switch, not a checkbox: a boolean column is a setting that is on or off,
 * and a switch says so at a glance. A checkbox reads as "include this one" and
 * carries selection-list baggage from tables and multi-select, which is where
 * Anubis uses checkboxes instead.
 */
export function BooleanField<Values extends FieldValues>(props: AnubisFieldProps<Values>) {
  const { fieldId, field, errorMessage } = useFieldState(props)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Switch
      id={fieldId}
      ref={field.ref}
      name={field.name}
      aria-label={props.label}
      isDisabled={props.isDisabled || props.isReadOnly}
      isSelected={Boolean(field.value)}
      onValueChange={field.onChange}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}
