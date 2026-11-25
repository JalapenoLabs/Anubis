// Copyright © 2026 Jalapeno Labs

import type { ResourceGrantDraft, ResourceGrantMode } from './draft'

// Core
import { useEffect, useRef, useState } from 'react'

// User interface
import { Autocomplete, AutocompleteItem, Chip, Radio, RadioGroup } from '@heroui/react'

/**
 * How long typing has to pause before `onSearch` is called.
 *
 * The same pause the field library's pickers wait, so every search box in an
 * application answers the keyboard alike.
 */
const SEARCH_DEBOUNCE_MS = 250

/**
 * The editor's own strings.
 *
 * The package never imports i18next, so the defaults are English and say
 * "record"; a page passes the kind's own words ("Every machine").
 */
export type ResourceGrantEditorLabels = {
  /** The choice's accessible name, usually the team and the kind. */
  group: string
  nothing: string
  every: string
  only: string
  /** The picker's accessible name and placeholder. */
  add: string
  /** The accessible name of the button that takes one picked record away. */
  remove: string
}

type Props = {
  draft: ResourceGrantDraft
  onChange: (draft: ResourceGrantDraft) => void
  /** The organization's records the picker offers for the current search. */
  options: { value: string, label: string }[]
  /** Called with the typed query, debounced, so the page can fetch its page. */
  onSearch: (query: string) => void
  isLoading?: boolean
  isDisabled?: boolean
  /** Why the viewer may not change this, shown beneath the locked choice. */
  reason?: string
  labels?: Partial<ResourceGrantEditorLabels>
}

const DEFAULT_LABELS: ResourceGrantEditorLabels = {
  group: 'Access',
  nothing: 'Nothing',
  every: 'Every record',
  only: 'Only these',
  add: 'Add a record',
  remove: 'Remove',
}

const MODES: readonly ResourceGrantMode[] = [ 'nothing', 'every', 'only' ]

/**
 * What one team reaches of one kind of the organization's own records.
 *
 * Three choices, because those are the three end states a team can be in:
 * nothing of the kind, every record of it (including the ones added later),
 * or specific records picked by name. A viewer who may not change it sees the
 * choice locked with the reason beneath it, never hidden, so they can tell
 * what holds them back. The editor holds no state of its own beyond the
 * search box: the page owns the draft and decides when to save it.
 */
export function ResourceGrantEditor(props: Props) {
  const labels = { ...DEFAULT_LABELS, ...props.labels }
  const [ searchText, setSearchText ] = useState('')

  // The page already has its first page of options, so the empty query the
  // box starts on is not a search anybody asked for.
  const hasTyped = useRef(false)
  const onSearch = props.onSearch
  useEffect(() => {
    if (!hasTyped.current && !searchText) {
      return undefined
    }
    hasTyped.current = true

    const timer = setTimeout(() => onSearch(searchText), SEARCH_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [ onSearch, searchText ])

  const picked = new Set(props.draft.resources.map((resource) => resource.value))
  const unpicked = props.options.filter((option) => !picked.has(option.value))

  return <div>
    <RadioGroup
      aria-label={labels.group}
      orientation='horizontal'
      value={props.draft.mode}
      isDisabled={props.isDisabled}
      onValueChange={(mode) => {
        const chosen = MODES.find((candidate) => candidate === mode)
        if (!chosen) {
          console.debug('ResourceGrantEditor ignored an unknown choice', { mode })
          return
        }
        props.onChange({ ...props.draft, mode: chosen })
      }}
    >
      <Radio value='nothing'>{
          labels.nothing
        }</Radio>
      <Radio value='every'>{
          labels.every
        }</Radio>
      <Radio value='only'>{
          labels.only
        }</Radio>
    </RadioGroup>
    { props.draft.mode === 'only'
      ? <div className='mt-2'>
          { props.draft.resources.length
            ? <div className='mb-2 flex flex-wrap gap-1'>
                { props.draft.resources.map((resource) => <Chip
                    key={resource.value}
                    variant='flat'
                    isDisabled={props.isDisabled}
                    classNames={{ closeButton: 'ml-1' }}
                    onClose={props.isDisabled
                      ? undefined
                      : () => props.onChange({
                          ...props.draft,
                          resources: props.draft.resources.filter(
                            (kept) => kept.value !== resource.value,
                          ),
                        })}
                    aria-label={`${labels.remove} ${resource.label}`}
                  >{
                    resource.label
                  }</Chip>)
                }
              </div>
            : null
          }
          <Autocomplete
            aria-label={labels.add}
            placeholder={labels.add}
            className='w-full max-w-md'
            isDisabled={props.isDisabled}
            isLoading={props.isLoading}
            selectedKey={null}
            inputValue={searchText}
            onInputChange={setSearchText}
            onSelectionChange={(key) => {
              // Clearing the box selects nothing, which picks nothing.
              if (key == null) {
                return
              }
              const option = props.options.find((candidate) => candidate.value === key)
              if (!option) {
                console.debug('ResourceGrantEditor picked a record its options no longer hold', { key })
                return
              }
              props.onChange({ ...props.draft, resources: [ ...props.draft.resources, option ]})
              setSearchText('')
            }}
          >
            { unpicked.map((option) => <AutocompleteItem key={option.value}>{
                option.label
              }</AutocompleteItem>)
            }
          </Autocomplete>
        </div>
      : null
    }
    { props.reason
      ? <p className='mt-1 text-small opacity-60'>{
          props.reason
        }</p>
      : null
    }
  </div>
}
