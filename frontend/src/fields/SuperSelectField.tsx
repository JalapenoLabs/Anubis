// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, FieldOption } from './types'
import type { FieldValues } from 'react-hook-form'

// Core
import { useEffect, useRef, useState } from 'react'

// User interface
import { Autocomplete, AutocompleteItem, Chip } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * How long typing has to pause before `onSearch` is called.
 *
 * Long enough that a word is one request rather than five, short enough that
 * the list feels attached to the keyboard.
 */
const SEARCH_DEBOUNCE_MS = 250

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  options: FieldOption[]
  /**
   * Multiple selection holds a `string[]` and renders the chosen options as
   * removable chips above the search box. Single selection holds a `string`.
   */
  isMultiple?: boolean
  /**
   * Called with the typed query, debounced, when the option list is fetched
   * rather than held. Leave it out and the given options are filtered in the
   * browser, which is the right answer for a list that fits in one response.
   */
  onSearch?: (query: string) => void
  /** Renders the loading indicator while a search is in flight. */
  isLoading?: boolean
}

/**
 * The `super_select` scaffolder type: a searchable association picker.
 *
 * This is the field an association lands in, so the options are the records the
 * scaffolder's `valid_*` scoping method returns for the current team. A
 * generated form binds them through the options hook `anubis scaffold join`
 * writes beside the association's other route functions, so this component
 * stays a pure control: it renders the list it is given.
 *
 * A list too long to send at once is the case `onSearch` exists for: the form
 * keeps the query in its own state, refetches the options endpoint with it,
 * and passes the page back down. The component's contract does not change,
 * which is the point: a directory-sized association and a five-row one render
 * through the same control.
 */
export function SuperSelectField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  // The multiple variant clears the search box after each pick; both variants
  // feed the debounced search when the form asked for one.
  const [ searchText, setSearchText ] = useState('')

  // The form already has its first page of options, so the empty query the
  // box starts on is not a search anybody asked for.
  const hasTyped = useRef(false)
  const onSearch = props.onSearch
  useEffect(() => {
    if (!onSearch || (!hasTyped.current && !searchText)) {
      return undefined
    }
    hasTyped.current = true

    const timer = setTimeout(() => onSearch(searchText), SEARCH_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [ onSearch, searchText ])

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
        isLoading={props.isLoading}
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
      isLoading={props.isLoading}
      selectedKey={field.value == null ? null : String(field.value)}
      onSelectionChange={(key) => field.onChange(key == null ? null : String(key))}
      // Listened to rather than controlled: the control keeps showing the
      // chosen option's label, and the query still reaches `onSearch`.
      onInputChange={setSearchText}
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
