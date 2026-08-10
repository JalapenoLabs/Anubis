// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, FieldOption } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Radio, RadioGroup, Select, SelectItem } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  options: FieldOption[]
  /**
   * `select` (the default) collapses the list into a dropdown; `radio` keeps
   * every choice on screen, which suits short lists and option descriptions.
   */
  variant?: 'select' | 'radio'
  /** Radio layout. Ignored by the select variant. */
  orientation?: 'vertical' | 'horizontal'
}

/**
 * The `options` scaffolder type: one value from a fixed list that ships with the
 * model, in either of the two shapes a single choice takes.
 */
export function OptionsField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  const selectedValue = field.value == null ? '' : String(field.value)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    { props.variant === 'radio'
      ? <RadioGroup
          id={fieldId}
          name={field.name}
          aria-label={props.label}
          orientation={props.orientation}
          isRequired={props.isRequired}
          isDisabled={props.isDisabled || props.isReadOnly}
          isInvalid={isInvalid}
          value={selectedValue}
          onValueChange={field.onChange}
          onBlur={field.onBlur}
        >
          { props.options.map((option) => <Radio
              key={option.value}
              value={option.value}
              description={option.description}
              isDisabled={option.isDisabled}
            >{
              option.label
            }</Radio>)
          }
        </RadioGroup>
      : <Select
          id={fieldId}
          name={field.name}
          aria-label={props.label}
          className='w-full'
          placeholder={props.placeholder}
          isRequired={props.isRequired}
          isDisabled={props.isDisabled || props.isReadOnly}
          isInvalid={isInvalid}
          selectedKeys={selectedValue ? [ selectedValue ] : []}
          onSelectionChange={(keys) => {
            if (keys === 'all') {
              console.debug('OptionsField ignored a select-all selection; it is a single choice', props.name)
              return
            }
            const [ firstKey ] = Array.from(keys)
            field.onChange(firstKey == null ? null : String(firstKey))
          }}
          onBlur={field.onBlur}
        >
          { props.options.map((option) => <SelectItem
              key={option.value}
              description={option.description}
              isDisabled={option.isDisabled}
            >{
              option.label
            }</SelectItem>)
          }
        </Select>
    }
  </FieldWrapper>
}
