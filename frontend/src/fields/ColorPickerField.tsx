// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

const HEX_COLOR_PATTERN = /^#[0-9a-f]{6}$/i
const FALLBACK_SWATCH_COLOR = '#000000'

/**
 * The `color_picker` scaffolder type: a `#rrggbb` value, editable as text or
 * through the platform's own color picker.
 *
 * The native picker is the swatch on the left; the text input carries the
 * accessible name and keeps the field fully operable from the keyboard, so the
 * swatch stays out of the tab order. A half-typed hex leaves the swatch on its
 * last valid color rather than flickering through nonsense.
 */
export function ColorPickerField<Values extends FieldValues>(props: AnubisFieldProps<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  const hexValue = field.value == null ? '' : String(field.value)
  const swatchValue = HEX_COLOR_PATTERN.test(hexValue)
    ? hexValue
    : FALLBACK_SWATCH_COLOR

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
      type='text'
      aria-label={props.label}
      className='w-full'
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={hexValue}
      onValueChange={field.onChange}
      onBlur={field.onBlur}
      startContent={
        <input
          type='color'
          aria-hidden='true'
          tabIndex={-1}
          className='h-6 w-6 cursor-pointer rounded border-0 bg-transparent p-0'
          disabled={props.isDisabled || props.isReadOnly}
          value={swatchValue}
          onChange={(event) => field.onChange(event.currentTarget.value)}
        />
      }
    />
  </FieldWrapper>
}
