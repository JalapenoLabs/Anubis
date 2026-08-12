// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, FieldOption } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Button, ButtonGroup } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  options: FieldOption[]
}

/**
 * The `buttons` scaffolder type: a segmented single choice.
 *
 * Use it for a handful of mutually exclusive values that deserve to stay
 * visible. Longer lists belong in `OptionsField`, and searchable lists in
 * `SuperSelectField`.
 */
export function ButtonsField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage } = useFieldState(props)

  const selectedValue = field.value == null ? '' : String(field.value)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <ButtonGroup
      id={fieldId}
      role='radiogroup'
      aria-label={props.label}
      aria-required={props.isRequired}
    >
      { props.options.map((option) => {
          const isSelected = option.value === selectedValue

          return <Button
            key={option.value}
            role='radio'
            aria-checked={isSelected}
            color={isSelected ? 'primary' : 'default'}
            variant={isSelected ? 'solid' : 'bordered'}
            isDisabled={props.isDisabled || props.isReadOnly || option.isDisabled}
            onPress={() => field.onChange(option.value)}
          >
            <span>{option.label}</span>
          </Button>
        })
      }
    </ButtonGroup>
  </FieldWrapper>
}
