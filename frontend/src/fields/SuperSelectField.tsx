// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, FieldOption } from './types'
import type { FieldValues } from 'react-hook-form'

// Core
import { useState } from 'react'

// User interface
import { Autocomplete, AutocompleteItem, Chip } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  options: FieldOption[]
  /**
   * Multiple selection holds a `string[]` and renders the chosen options as
   * removable chips above the search box. Single selection holds a `string`.
   */
  isMultiple?: boolean
}

/**
 * The `super_select` scaffolder type: a searchable association picker.
 *
 * This is the field an association lands in, so the options are the records the
 * scaffolder's `valid_*` scoping method returns for the current team. A
 * generated form binds them through the options hook `anubis scaffold join`
 * writes beside the association's other route functions, so this component
 * stays a pure control: it renders the list it is given.
 */
export function SuperSelectField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  // Only the multiple variant needs to clear the search box after each pick.
  const [ searchText, setSearchText ] = useState('')

  if (props.isMultiple) {
    const selectedValues: string[] = Array.isArray(field.value)
      ? field.value.map(String)
      : []

    const labelByValue = new Map(props.options.map((option) => [ option.value, option.label ]))
    const unselectedOptions = props.options.filter((option) => !selectedValues.includes(option.value))

    return <FieldWrapper
      htmlFor={fieldId}
      label={props.label}
      isRequired={props.isRequired}
      help={props.help}
      error={errorMessage}
      className={props.className}
    >
      { selectedValues.length
        ? <div className='mb-1 flex flex-wrap gap-1'>
            { selectedValues.map((value) => <Chip
                key={value}
                variant='flat'
                isDisabled={props.isDisabled}
                onClose={
                  props.isReadOnly || props.isDisabled
                    ? undefined
                    : () => field.onChange(selectedValues.filter((kept) => kept !== value))
                }
              >{
                labelByValue.get(value) ?? value
              }</Chip>)
            }
          </div>
        : null
      }
      <Autocomplete
        id={fieldId}
        aria-label={props.label}
        className='w-full'
        placeholder={props.placeholder}
        autoFocus={props.autoFocus}
        isRequired={props.isRequired}
        isDisabled={props.isDisabled || props.isReadOnly}
        isInvalid={isInvalid}
        selectedKey={null}
        inputValue={searchText}
        onInputChange={setSearchText}
        onSelectionChange={(key) => {
          if (key == null) {
            return
          }
          field.onChange([ ...selectedValues, String(key) ])
          setSearchText('')
        }}
        onBlur={field.onBlur}
      >
        { unselectedOptions.map((option) => <AutocompleteItem
            key={option.value}
            description={option.description}
            isDisabled={option.isDisabled}
          >{
            option.label
          }</AutocompleteItem>)
        }
      </Autocomplete>
    </FieldWrapper>
  }

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Autocomplete
      id={fieldId}
      aria-label={props.label}
      className='w-full'
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled || props.isReadOnly}
      isInvalid={isInvalid}
      selectedKey={field.value == null ? null : String(field.value)}
      onSelectionChange={(key) => field.onChange(key == null ? null : String(key))}
      onBlur={field.onBlur}
    >
      { props.options.map((option) => <AutocompleteItem
          key={option.value}
          description={option.description}
          isDisabled={option.isDisabled}
        >{
          option.label
        }</AutocompleteItem>)
      }
    </Autocomplete>
  </FieldWrapper>
}
